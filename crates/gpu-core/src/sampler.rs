use crate::error::{GpuError, Result};
use crate::model::{SampleRequest, UnavailableReason};
use crate::monitor::MonitorInner;
use crate::snapshot::GpuSnapshot;
use parking_lot::{Condvar, Mutex};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MIN_INTERVAL_MS: u64 = 50;
const MAX_INTERVAL_MS: u64 = 60_000;
const SNAPSHOT_COALESCE_MS: u64 = 10;

#[derive(Debug, Clone)]
pub struct WatchOptions {
    pub interval_ms: u64,
    pub include_processes: bool,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            interval_ms: 1_000,
            include_processes: false,
        }
    }
}

enum Command {
    Sample {
        device_id: String,
        request: SampleRequest,
        response: SyncSender<Result<GpuSnapshot>>,
    },
    Subscribe {
        id: u64,
        device_id: String,
        options: WatchOptions,
        slot: Arc<LatestSlot>,
    },
    Unsubscribe {
        id: u64,
    },
    Refresh {
        response: SyncSender<Result<()>>,
    },
    Shutdown {
        response: SyncSender<()>,
    },
}

struct SubscriptionEntry {
    device_id: String,
    options: WatchOptions,
    next_due: Instant,
    slot: Arc<LatestSlot>,
}

#[derive(Default)]
struct LatestState {
    sequence: u64,
    delivered_sequence: u64,
    latest: Option<GpuSnapshot>,
    error: Option<String>,
    closed: bool,
}

#[derive(Default)]
struct LatestSlot {
    state: Mutex<LatestState>,
    changed: Condvar,
}

impl LatestSlot {
    fn publish(&self, snapshot: GpuSnapshot) {
        let mut state = self.state.lock();
        state.sequence = state.sequence.wrapping_add(1);
        state.latest = Some(snapshot);
        state.error = None;
        self.changed.notify_all();
    }

    fn fail(&self, error: impl Into<String>) {
        let mut state = self.state.lock();
        state.sequence = state.sequence.wrapping_add(1);
        state.error = Some(error.into());
        self.changed.notify_all();
    }

    fn close(&self) {
        let mut state = self.state.lock();
        state.closed = true;
        self.changed.notify_all();
    }
}

pub struct SampleSubscription {
    id: u64,
    commands: Sender<Command>,
    slot: Arc<LatestSlot>,
    cancelled: AtomicBool,
}

impl SampleSubscription {
    pub fn next(&self) -> Result<Option<GpuSnapshot>> {
        let mut state = self.slot.state.lock();
        while state.sequence == state.delivered_sequence && !state.closed {
            self.slot.changed.wait(&mut state);
        }
        if state.closed && state.sequence == state.delivered_sequence {
            return Ok(None);
        }
        state.delivered_sequence = state.sequence;
        if let Some(message) = state.error.clone() {
            return Err(GpuError::Internal(message));
        }
        Ok(state.latest.clone())
    }

    pub fn cancel(&self) {
        if self.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        self.slot.close();
        let _ = self.commands.send(Command::Unsubscribe { id: self.id });
    }
}

impl Drop for SampleSubscription {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub(crate) struct SamplerHub {
    commands: Sender<Command>,
    thread: Mutex<Option<JoinHandle<()>>>,
    next_subscription_id: AtomicU64,
    stopped: AtomicBool,
}

impl SamplerHub {
    pub(crate) fn start(monitor: Weak<MonitorInner>) -> Result<Self> {
        let (commands, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("let-smi-sampler".into())
            .spawn(move || run_sampler(monitor, receiver))
            .map_err(|error| {
                GpuError::Internal(format!("failed to start GPU sampler thread: {error}"))
            })?;
        Ok(Self {
            commands,
            thread: Mutex::new(Some(thread)),
            next_subscription_id: AtomicU64::new(1),
            stopped: AtomicBool::new(false),
        })
    }

    pub(crate) fn sample(&self, device_id: String, request: SampleRequest) -> Result<GpuSnapshot> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(GpuError::MonitorClosed);
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Sample {
                device_id,
                request,
                response: sender,
            })
            .map_err(|_| GpuError::MonitorClosed)?;
        receiver.recv().map_err(|_| GpuError::MonitorClosed)?
    }

    pub(crate) fn subscribe(
        &self,
        device_id: String,
        mut options: WatchOptions,
    ) -> Result<SampleSubscription> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(GpuError::MonitorClosed);
        }
        options.interval_ms = options.interval_ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS);
        let id = self.next_subscription_id.fetch_add(1, Ordering::Relaxed);
        let slot = Arc::new(LatestSlot::default());
        self.commands
            .send(Command::Subscribe {
                id,
                device_id,
                options,
                slot: Arc::clone(&slot),
            })
            .map_err(|_| GpuError::MonitorClosed)?;
        Ok(SampleSubscription {
            id,
            commands: self.commands.clone(),
            slot,
            cancelled: AtomicBool::new(false),
        })
    }

    pub(crate) fn refresh(&self) -> Result<()> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(GpuError::MonitorClosed);
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Refresh { response: sender })
            .map_err(|_| GpuError::MonitorClosed)?;
        receiver.recv().map_err(|_| GpuError::MonitorClosed)?
    }

    pub(crate) fn shutdown(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        if self
            .commands
            .send(Command::Shutdown { response: sender })
            .is_ok()
        {
            let _ = receiver.recv_timeout(Duration::from_secs(5));
        }
        if let Some(thread) = self.thread.lock().take()
            && thread.thread().id() != thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

impl Drop for SamplerHub {
    fn drop(&mut self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        let (sender, _receiver) = mpsc::sync_channel(1);
        let _ = self.commands.send(Command::Shutdown { response: sender });
        if let Some(thread) = self.thread.get_mut().take()
            && thread.thread().id() != thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

fn run_sampler(monitor: Weak<MonitorInner>, receiver: Receiver<Command>) {
    let mut subscriptions: HashMap<u64, SubscriptionEntry> = HashMap::new();
    let mut latest_snapshots: HashMap<(String, bool), (Instant, GpuSnapshot)> = HashMap::new();

    loop {
        if subscriptions
            .values()
            .any(|entry| entry.next_due <= Instant::now())
        {
            sample_due_subscriptions(&monitor, &mut subscriptions, &mut latest_snapshots);
        }
        let timeout = subscriptions
            .values()
            .map(|entry| entry.next_due.saturating_duration_since(Instant::now()))
            .min()
            .unwrap_or(Duration::from_secs(60));

        match receiver.recv_timeout(timeout) {
            Ok(Command::Sample {
                device_id,
                request,
                response,
            }) => {
                let result = monitor.upgrade().map_or_else(
                    || Err(GpuError::MonitorClosed),
                    |monitor| sample_with_optional_warmup(&monitor, &device_id, &request),
                );
                let _ = response.send(result);
            }
            Ok(Command::Subscribe {
                id,
                device_id,
                options,
                slot,
            }) => {
                subscriptions.insert(
                    id,
                    SubscriptionEntry {
                        device_id,
                        options,
                        next_due: Instant::now(),
                        slot,
                    },
                );
            }
            Ok(Command::Unsubscribe { id }) => {
                if let Some(entry) = subscriptions.remove(&id) {
                    entry.slot.close();
                }
            }
            Ok(Command::Refresh { response }) => {
                let result = monitor.upgrade().map_or_else(
                    || Err(GpuError::MonitorClosed),
                    |monitor| monitor.refresh_devices(),
                );
                latest_snapshots.clear();
                let _ = response.send(result);
            }
            Ok(Command::Shutdown { response }) => {
                for entry in subscriptions.into_values() {
                    entry.slot.close();
                }
                if let Some(monitor) = monitor.upgrade() {
                    monitor.shutdown_providers();
                }
                let _ = response.send(());
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                for entry in subscriptions.into_values() {
                    entry.slot.close();
                }
                break;
            }
        }
    }
}

fn sample_due_subscriptions(
    monitor: &Weak<MonitorInner>,
    subscriptions: &mut HashMap<u64, SubscriptionEntry>,
    latest_snapshots: &mut HashMap<(String, bool), (Instant, GpuSnapshot)>,
) {
    let now = Instant::now();
    let due: Vec<u64> = subscriptions
        .iter()
        .filter_map(|(id, entry)| (entry.next_due <= now).then_some(*id))
        .collect();
    let Some(monitor) = monitor.upgrade() else {
        for entry in subscriptions.values() {
            entry.slot.close();
        }
        subscriptions.clear();
        return;
    };

    for id in due {
        let Some(entry) = subscriptions.get_mut(&id) else {
            continue;
        };
        let key = (entry.device_id.clone(), entry.options.include_processes);
        let cached = latest_snapshots
            .get(&key)
            .filter(|(sampled_at, _)| {
                sampled_at.elapsed() <= Duration::from_millis(SNAPSHOT_COALESCE_MS)
            })
            .map(|(_, snapshot)| snapshot.clone());
        let result = if let Some(snapshot) = cached {
            Ok(snapshot)
        } else {
            let result = monitor.sample_once(
                &entry.device_id,
                &SampleRequest {
                    window_ms: 0,
                    metrics: None,
                    include_processes: entry.options.include_processes,
                },
            );
            if let Ok(snapshot) = &result {
                latest_snapshots.insert(key, (Instant::now(), snapshot.clone()));
            }
            result
        };
        match result {
            Ok(snapshot) => entry.slot.publish(snapshot),
            Err(error) => entry.slot.fail(error.to_string()),
        }
        let interval = Duration::from_millis(entry.options.interval_ms);
        while entry.next_due <= now {
            entry.next_due += interval;
        }
    }
}

fn sample_with_optional_warmup(
    monitor: &MonitorInner,
    device_id: &str,
    request: &SampleRequest,
) -> Result<GpuSnapshot> {
    let first = monitor.sample_once(device_id, request)?;
    let needs_warmup = snapshot_has_first_sample(&first);
    if !needs_warmup || request.window_ms == 0 {
        return Ok(first);
    }

    let deadline = Instant::now() + Duration::from_millis(request.window_ms);
    while Instant::now() < deadline {
        if monitor.is_closed() {
            return Err(GpuError::MonitorClosed);
        }
        thread::sleep(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(50)),
        );
    }
    monitor.sample_once(device_id, request)
}

fn snapshot_has_first_sample(snapshot: &GpuSnapshot) -> bool {
    matches!(
        snapshot.utilization.overall,
        crate::model::Metric::Unavailable(ref metric)
            if metric.reason == UnavailableReason::FirstSample
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_interval_is_bounded() {
        assert_eq!(0_u64.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS), 50);
        assert_eq!(u64::MAX.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS), 60_000);
    }
}
