use crate::error::{GpuError, Result};
use crate::model::{SampleRequest, UnavailableReason};
use crate::monitor::MonitorInner;
use crate::snapshot::GpuSnapshot;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Weak};
use std::task::{Context, Poll, Waker};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MIN_INTERVAL_MS: u64 = 50;
const MAX_INTERVAL_MS: u64 = 60_000;
const SNAPSHOT_COALESCE_MS: u64 = 10;
pub(crate) const COMMAND_QUEUE_CAPACITY: usize = 256;
pub(crate) const MAX_SUBSCRIPTIONS: usize = 128;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

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
    waiter: Option<Waker>,
}

#[derive(Default)]
struct LatestSlot {
    state: Mutex<LatestState>,
}

impl LatestSlot {
    fn publish(&self, snapshot: GpuSnapshot) {
        let waiter = {
            let mut state = self.state.lock();
            if state.closed {
                return;
            }
            state.sequence = state.sequence.wrapping_add(1);
            state.latest = Some(snapshot);
            state.error = None;
            state.waiter.take()
        };
        if let Some(waiter) = waiter {
            waiter.wake();
        }
    }

    fn fail(&self, error: impl Into<String>) {
        let waiter = {
            let mut state = self.state.lock();
            if state.closed {
                return;
            }
            state.sequence = state.sequence.wrapping_add(1);
            state.latest = None;
            state.error = Some(error.into());
            state.waiter.take()
        };
        if let Some(waiter) = waiter {
            waiter.wake();
        }
    }

    fn close(&self) {
        let waiter = {
            let mut state = self.state.lock();
            if state.closed {
                return;
            }
            state.closed = true;
            state.delivered_sequence = state.sequence;
            state.latest = None;
            state.error = None;
            state.waiter.take()
        };
        if let Some(waiter) = waiter {
            waiter.wake();
        }
    }

    fn is_closed(&self) -> bool {
        self.state.lock().closed
    }

    fn clear_waiter(&self) {
        self.state.lock().waiter = None;
    }

    fn poll_next(&self, context: &mut Context<'_>) -> Poll<Result<Option<GpuSnapshot>>> {
        let mut state = self.state.lock();
        if state.closed {
            return Poll::Ready(Ok(None));
        }
        if state.sequence != state.delivered_sequence {
            state.delivered_sequence = state.sequence;
            if let Some(message) = state.error.clone() {
                return Poll::Ready(Err(GpuError::Internal(message)));
            }
            return Poll::Ready(Ok(state.latest.clone()));
        }
        let replace_waiter = state
            .waiter
            .as_ref()
            .is_none_or(|waiter| !waiter.will_wake(context.waker()));
        if replace_waiter {
            state.waiter = Some(context.waker().clone());
        }
        Poll::Pending
    }
}

pub struct SampleSubscription {
    id: u64,
    commands: SyncSender<Command>,
    registry: Arc<Mutex<HashMap<u64, Weak<LatestSlot>>>>,
    slot: Arc<LatestSlot>,
    cancelled: AtomicBool,
    next_in_flight: Arc<AtomicBool>,
}

impl SampleSubscription {
    pub fn next_async(&self) -> Result<NextSampleFuture> {
        self.next_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                GpuError::InvalidArgument(
                    "only one next() call may be in flight per native subscription".into(),
                )
            })?;
        Ok(NextSampleFuture {
            slot: Arc::clone(&self.slot),
            next_in_flight: Arc::clone(&self.next_in_flight),
            completed: false,
        })
    }

    pub fn cancel(&self) {
        if self.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        self.slot.close();
        self.registry.lock().remove(&self.id);
        let _ = self.commands.try_send(Command::Unsubscribe { id: self.id });
    }
}

impl Drop for SampleSubscription {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub struct NextSampleFuture {
    slot: Arc<LatestSlot>,
    next_in_flight: Arc<AtomicBool>,
    completed: bool,
}

impl Future for NextSampleFuture {
    type Output = Result<Option<GpuSnapshot>>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this.slot.poll_next(context) {
            Poll::Ready(value) => {
                this.completed = true;
                this.next_in_flight.store(false, Ordering::Release);
                Poll::Ready(value)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for NextSampleFuture {
    fn drop(&mut self) {
        if !self.completed {
            self.slot.clear_waiter();
            self.next_in_flight.store(false, Ordering::Release);
        }
    }
}

pub(crate) struct SamplerHub {
    commands: SyncSender<Command>,
    thread: Mutex<Option<JoinHandle<()>>>,
    registry: Arc<Mutex<HashMap<u64, Weak<LatestSlot>>>>,
    stop_requested: Arc<AtomicBool>,
    next_subscription_id: AtomicU64,
    stopped: AtomicBool,
}

impl SamplerHub {
    pub(crate) fn start(monitor: Arc<MonitorInner>) -> Result<Self> {
        let (commands, receiver) = mpsc::sync_channel(COMMAND_QUEUE_CAPACITY);
        let stop_requested = Arc::new(AtomicBool::new(false));
        let sampler_stop = Arc::clone(&stop_requested);
        let thread = thread::Builder::new()
            .name("let-smi-sampler".into())
            .spawn(move || run_sampler(monitor, receiver, sampler_stop))
            .map_err(|error| {
                GpuError::Internal(format!("failed to start GPU sampler thread: {error}"))
            })?;
        Ok(Self {
            commands,
            thread: Mutex::new(Some(thread)),
            registry: Arc::new(Mutex::new(HashMap::new())),
            stop_requested,
            next_subscription_id: AtomicU64::new(1),
            stopped: AtomicBool::new(false),
        })
    }

    pub(crate) fn sample(&self, device_id: String, request: SampleRequest) -> Result<GpuSnapshot> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(GpuError::MonitorClosed);
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        self.try_send(Command::Sample {
            device_id,
            request,
            response: sender,
        })?;
        self.wait_for_response(receiver)
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
        {
            let mut registry = self.registry.lock();
            // Shutdown sets `stopped` before draining this same registry. The
            // in-lock recheck closes the race between the fast-path check
            // above and registration of a new pending consumer.
            if self.stopped.load(Ordering::Acquire) {
                return Err(GpuError::MonitorClosed);
            }
            registry.retain(|_, slot| slot.strong_count() > 0);
            if registry.len() >= MAX_SUBSCRIPTIONS {
                return Err(GpuError::Backpressure(format!(
                    "native subscription limit ({MAX_SUBSCRIPTIONS}) reached"
                )));
            }
            registry.insert(id, Arc::downgrade(&slot));
        }
        if let Err(error) = self.try_send(Command::Subscribe {
            id,
            device_id,
            options,
            slot: Arc::clone(&slot),
        }) {
            self.registry.lock().remove(&id);
            slot.close();
            return Err(error);
        }
        Ok(SampleSubscription {
            id,
            commands: self.commands.clone(),
            registry: Arc::clone(&self.registry),
            slot,
            cancelled: AtomicBool::new(false),
            next_in_flight: Arc::new(AtomicBool::new(false)),
        })
    }

    pub(crate) fn refresh(&self) -> Result<()> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(GpuError::MonitorClosed);
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        self.try_send(Command::Refresh { response: sender })?;
        self.wait_for_response(receiver)
    }

    pub(crate) fn shutdown(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        self.close_all_slots();
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        // Signal first so the sampler abandons queued ordinary work at its
        // next loop boundary. The command only wakes an idle recv_timeout and
        // provides the direct normal shutdown path when it wins the race.
        self.stop_requested.store(true, Ordering::Release);
        let (sender, receiver) = mpsc::sync_channel(1);
        let _ = self
            .commands
            .try_send(Command::Shutdown { response: sender });
        let _ = receiver.recv_timeout(
            Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
        );
        self.finish_thread_until(deadline);
    }

    pub(crate) fn request_shutdown_nonblocking(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        self.close_all_slots();
        self.stop_requested.store(true, Ordering::Release);
        let (sender, _receiver) = mpsc::sync_channel(1);
        let _ = self
            .commands
            .try_send(Command::Shutdown { response: sender });
        let _ = self.thread.lock().take();
    }

    fn try_send(&self, command: Command) -> Result<()> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    GpuError::Backpressure("sampler command queue is full".into())
                }
                TrySendError::Disconnected(_) => GpuError::MonitorClosed,
            })
    }

    fn wait_for_response<T>(&self, receiver: Receiver<Result<T>>) -> Result<T> {
        loop {
            match receiver.try_recv() {
                Ok(value) => return value,
                Err(TryRecvError::Disconnected) => return Err(GpuError::MonitorClosed),
                Err(TryRecvError::Empty) if self.stopped.load(Ordering::Acquire) => {
                    return Err(GpuError::MonitorClosed);
                }
                Err(TryRecvError::Empty) => {}
            }
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(value) => return value,
                Err(RecvTimeoutError::Disconnected) => return Err(GpuError::MonitorClosed),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn close_all_slots(&self) {
        let slots: Vec<_> = self
            .registry
            .lock()
            .drain()
            .filter_map(|(_, slot)| slot.upgrade())
            .collect();
        for slot in slots {
            slot.close();
        }
    }

    fn finish_thread_until(&self, deadline: Instant) {
        let Some(thread) = self.thread.lock().take() else {
            return;
        };
        if thread.thread().id() == thread::current().id() {
            return;
        }
        while !thread.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        if thread.is_finished() {
            let _ = thread.join();
        }
        // Dropping an unfinished JoinHandle detaches it. This is the bounded
        // exceptional path for a provider call that does not return.
    }
}

impl Drop for SamplerHub {
    fn drop(&mut self) {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            let slots: Vec<_> = self
                .registry
                .lock()
                .drain()
                .filter_map(|(_, slot)| slot.upgrade())
                .collect();
            for slot in slots {
                slot.close();
            }
            self.stop_requested.store(true, Ordering::Release);
            let (sender, _receiver) = mpsc::sync_channel(1);
            let _ = self
                .commands
                .try_send(Command::Shutdown { response: sender });
        }
        let _ = self.thread.get_mut().take();
    }
}

fn run_sampler(
    monitor: Arc<MonitorInner>,
    receiver: Receiver<Command>,
    stop_requested: Arc<AtomicBool>,
) {
    let mut subscriptions: HashMap<u64, SubscriptionEntry> = HashMap::new();
    let mut latest_snapshots: HashMap<(String, bool), (Instant, GpuSnapshot)> = HashMap::new();

    loop {
        subscriptions.retain(|_, entry| !entry.slot.is_closed());
        latest_snapshots.retain(|(device_id, include_processes), _| {
            subscriptions.values().any(|entry| {
                &entry.device_id == device_id
                    && entry.options.include_processes == *include_processes
            })
        });
        if stop_requested.load(Ordering::Acquire) {
            close_subscriptions(&mut subscriptions);
            monitor.shutdown_providers();
            break;
        }
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
            Ok(command)
                if stop_requested.load(Ordering::Acquire)
                    && !matches!(&command, Command::Shutdown { .. }) =>
            {
                // Drop any queued response sender instead of beginning new
                // provider work after close has been requested.
                close_subscriptions(&mut subscriptions);
                monitor.shutdown_providers();
                break;
            }
            Ok(Command::Sample {
                device_id,
                request,
                response,
            }) => {
                let result = sample_with_optional_warmup(&monitor, &device_id, &request);
                let _ = response.send(result);
            }
            Ok(Command::Subscribe {
                id,
                device_id,
                options,
                slot,
            }) => {
                if stop_requested.load(Ordering::Acquire) || slot.is_closed() {
                    slot.close();
                    continue;
                }
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
                let result = monitor.refresh_devices();
                latest_snapshots.clear();
                let _ = response.send(result);
            }
            Ok(Command::Shutdown { response }) => {
                close_subscriptions(&mut subscriptions);
                monitor.shutdown_providers();
                let _ = response.send(());
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                close_subscriptions(&mut subscriptions);
                monitor.shutdown_providers();
                break;
            }
        }
    }
}

fn close_subscriptions(subscriptions: &mut HashMap<u64, SubscriptionEntry>) {
    for (_, entry) in subscriptions.drain() {
        entry.slot.close();
    }
}

fn sample_due_subscriptions(
    monitor: &MonitorInner,
    subscriptions: &mut HashMap<u64, SubscriptionEntry>,
    latest_snapshots: &mut HashMap<(String, bool), (Instant, GpuSnapshot)>,
) {
    let now = Instant::now();
    let due: Vec<u64> = subscriptions
        .iter()
        .filter_map(|(id, entry)| (!entry.slot.is_closed() && entry.next_due <= now).then_some(*id))
        .collect();
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
    [
        Some(&snapshot.utilization.overall),
        snapshot.utilization.graphics.as_ref(),
        snapshot.utilization.compute.as_ref(),
        snapshot.utilization.copy.as_ref(),
        snapshot.utilization.memory_controller.as_ref(),
        snapshot.utilization.encoder.as_ref(),
        snapshot.utilization.decoder.as_ref(),
        snapshot.memory.dedicated_used_bytes.as_ref(),
        snapshot.memory.shared_used_bytes.as_ref(),
        snapshot.memory.unified_used_bytes.as_ref(),
        snapshot.memory.budget_bytes.as_ref(),
        snapshot.memory.bandwidth_utilization_percent.as_ref(),
        snapshot.temperatures.core_celsius.as_ref(),
        snapshot.temperatures.edge_celsius.as_ref(),
        snapshot.temperatures.hotspot_celsius.as_ref(),
        snapshot.temperatures.memory_celsius.as_ref(),
        snapshot.power.draw_watts.as_ref(),
        snapshot.power.limit_watts.as_ref(),
        snapshot.power.energy_joules.as_ref(),
        snapshot.clocks.graphics_mhz.as_ref(),
        snapshot.clocks.compute_mhz.as_ref(),
        snapshot.clocks.memory_mhz.as_ref(),
        snapshot.clocks.video_mhz.as_ref(),
        snapshot.fan.percent.as_ref(),
        snapshot.fan.rpm.as_ref(),
    ]
    .into_iter()
    .flatten()
    .any(|metric| {
        matches!(
            metric,
            crate::model::Metric::Unavailable(value)
                if value.reason == UnavailableReason::FirstSample
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(sampled_at: u64) -> GpuSnapshot {
        serde_json::from_value(serde_json::json!({
            "sampledAt": sampled_at,
            "utilization": {
                "overall": { "available": false, "reason": "unsupported" }
            },
            "memory": { "topology": "unknown" },
            "temperatures": {},
            "power": {},
            "clocks": {},
            "fan": {}
        }))
        .expect("valid test snapshot")
    }

    #[test]
    fn watch_interval_is_bounded() {
        assert_eq!(0_u64.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS), 50);
        assert_eq!(u64::MAX.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS), 60_000);
    }

    #[test]
    fn command_channel_is_bounded() {
        let (sender, _receiver) = mpsc::sync_channel(COMMAND_QUEUE_CAPACITY);
        for value in 0..COMMAND_QUEUE_CAPACITY {
            sender.try_send(value).unwrap();
        }
        assert!(matches!(
            sender.try_send(COMMAND_QUEUE_CAPACITY),
            Err(TrySendError::Full(_))
        ));
    }

    #[test]
    fn a_slow_consumer_receives_only_the_latest_snapshot() {
        let slot = LatestSlot::default();
        slot.publish(snapshot(1));
        slot.publish(snapshot(2));

        let mut context = Context::from_waker(Waker::noop());
        match slot.poll_next(&mut context) {
            Poll::Ready(Ok(Some(value))) => assert_eq!(value.sampled_at, 2),
            result => panic!("expected the latest coalesced snapshot, got {result:?}"),
        }
        assert!(matches!(slot.poll_next(&mut context), Poll::Pending));
    }

    #[test]
    fn only_one_next_future_can_be_in_flight() {
        let (commands, _receiver) = mpsc::sync_channel(1);
        let subscription = SampleSubscription {
            id: 1,
            commands,
            registry: Arc::new(Mutex::new(HashMap::new())),
            slot: Arc::new(LatestSlot::default()),
            cancelled: AtomicBool::new(false),
            next_in_flight: Arc::new(AtomicBool::new(false)),
        };
        let first = subscription.next_async().unwrap();
        assert!(matches!(
            subscription.next_async(),
            Err(GpuError::InvalidArgument(_))
        ));
        drop(first);
        assert!(subscription.next_async().is_ok());
    }
}
