use crate::error::{GpuError, Result};
use crate::model::{SampleRequest, UnavailableReason};
use crate::monitor::MonitorInner;
use crate::snapshot::{GpuDeviceSnapshot, GpuMonitorSnapshot, GpuSnapshot};
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
        device_id: Option<String>,
        request: SampleRequest,
        response: SyncSender<Result<GpuMonitorSnapshot>>,
    },
    Subscribe {
        id: u64,
        device_id: Option<String>,
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
    device_id: Option<String>,
    options: WatchOptions,
    next_due: Instant,
    slot: Arc<LatestSlot>,
}

#[derive(Default)]
struct LatestState {
    sequence: u64,
    delivered_sequence: u64,
    latest: Option<GpuMonitorSnapshot>,
    error: Option<String>,
    closed: bool,
    waiter: Option<Waker>,
}

#[derive(Default)]
struct LatestSlot {
    state: Mutex<LatestState>,
}

impl LatestSlot {
    fn publish(&self, snapshot: GpuMonitorSnapshot) {
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

    fn poll_next(&self, context: &mut Context<'_>) -> Poll<Result<Option<GpuMonitorSnapshot>>> {
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
    fn next_batch_async(&self) -> Result<NextBatchSampleFuture> {
        self.next_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                GpuError::InvalidArgument(
                    "only one next() call may be in flight per native subscription".into(),
                )
            })?;
        Ok(NextBatchSampleFuture {
            slot: Arc::clone(&self.slot),
            next_in_flight: Arc::clone(&self.next_in_flight),
            completed: false,
        })
    }

    pub fn next_async(&self) -> Result<NextSampleFuture> {
        Ok(NextSampleFuture {
            inner: self.next_batch_async()?,
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

pub struct NextBatchSampleFuture {
    slot: Arc<LatestSlot>,
    next_in_flight: Arc<AtomicBool>,
    completed: bool,
}

impl Future for NextBatchSampleFuture {
    type Output = Result<Option<GpuMonitorSnapshot>>;

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

impl Drop for NextBatchSampleFuture {
    fn drop(&mut self) {
        if !self.completed {
            self.slot.clear_waiter();
            self.next_in_flight.store(false, Ordering::Release);
        }
    }
}

pub struct BatchSampleSubscription {
    inner: SampleSubscription,
}

impl BatchSampleSubscription {
    pub fn next_async(&self) -> Result<NextBatchSampleFuture> {
        self.inner.next_batch_async()
    }
    pub fn cancel(&self) {
        self.inner.cancel();
    }
}

pub struct NextSampleFuture {
    inner: NextBatchSampleFuture,
}

impl Future for NextSampleFuture {
    type Output = Result<Option<GpuSnapshot>>;
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().inner)
            .poll(context)
            .map(|result| {
                result.and_then(|batch| {
                    batch
                        .map(|batch| {
                            batch
                                .gpus
                                .into_iter()
                                .next()
                                .map(|gpu| gpu.snapshot)
                                .ok_or_else(|| {
                                    GpuError::Internal("single-device delivery was empty".into())
                                })
                        })
                        .transpose()
                })
            })
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
        self.sample_target(Some(device_id), request)?
            .gpus
            .into_iter()
            .next()
            .map(|gpu| gpu.snapshot)
            .ok_or_else(|| GpuError::Internal("single-device sample was empty".into()))
    }

    pub(crate) fn sample_all(&self, request: SampleRequest) -> Result<GpuMonitorSnapshot> {
        self.sample_target(None, request)
    }

    fn sample_target(
        &self,
        device_id: Option<String>,
        request: SampleRequest,
    ) -> Result<GpuMonitorSnapshot> {
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
        options: WatchOptions,
    ) -> Result<SampleSubscription> {
        self.subscribe_target(Some(device_id), options)
    }

    pub(crate) fn subscribe_all(&self, options: WatchOptions) -> Result<BatchSampleSubscription> {
        Ok(BatchSampleSubscription {
            inner: self.subscribe_target(None, options)?,
        })
    }

    fn subscribe_target(
        &self,
        device_id: Option<String>,
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

struct PendingSample {
    device_id: Option<String>,
    request: SampleRequest,
    response: SyncSender<Result<GpuMonitorSnapshot>>,
    ready_at: Option<Instant>,
}

struct CachedSnapshot {
    collected_at: Instant,
    snapshot: GpuSnapshot,
    processes_collected: bool,
}

fn run_sampler(
    monitor: Arc<MonitorInner>,
    receiver: Receiver<Command>,
    stop_requested: Arc<AtomicBool>,
) {
    let mut subscriptions: HashMap<u64, SubscriptionEntry> = HashMap::new();
    let mut pending = Vec::new();
    let mut cache = HashMap::new();
    loop {
        subscriptions.retain(|_, entry| !entry.slot.is_closed());
        if stop_requested.load(Ordering::Acquire) {
            close_subscriptions(&mut subscriptions);
            monitor.shutdown_providers();
            break;
        }
        deliver_due(&monitor, &mut subscriptions, &mut pending, &mut cache);
        let now = Instant::now();
        let timeout = subscriptions
            .values()
            .map(|entry| entry.next_due)
            .chain(
                pending
                    .iter()
                    .map(|entry: &PendingSample| entry.ready_at.unwrap_or(now)),
            )
            .min()
            .map(|due| due.saturating_duration_since(now))
            .unwrap_or(Duration::from_secs(60));
        match receiver.recv_timeout(timeout) {
            Ok(command)
                if stop_requested.load(Ordering::Acquire)
                    && !matches!(&command, Command::Shutdown { .. }) =>
            {
                close_subscriptions(&mut subscriptions);
                monitor.shutdown_providers();
                break;
            }
            Ok(Command::Sample {
                device_id,
                request,
                response,
            }) => {
                if pending.len() >= COMMAND_QUEUE_CAPACITY {
                    let _ = response.send(Err(GpuError::Backpressure(
                        "pending sample limit reached".into(),
                    )));
                } else {
                    pending.push(PendingSample {
                        device_id,
                        request,
                        response,
                        ready_at: None,
                    });
                }
            }
            Ok(Command::Subscribe {
                id,
                device_id,
                options,
                slot,
            }) => {
                if slot.is_closed() {
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
                cache.clear();
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

fn target_ids(target: &Option<String>, inventory: &[String]) -> Vec<String> {
    target
        .as_ref()
        .map_or_else(|| inventory.to_vec(), |id| vec![id.clone()])
}

fn batch_for(
    monitor: &MonitorInner,
    target: &Option<String>,
    inventory: &[String],
    request: &SampleRequest,
    values: &HashMap<String, Result<GpuSnapshot>>,
) -> Result<GpuMonitorSnapshot> {
    let gpus = target_ids(target, inventory)
        .into_iter()
        .map(|id| {
            let mut snapshot = values
                .get(&id)
                .ok_or_else(|| GpuError::DeviceNotFound(id.clone()))?
                .clone()?;
            monitor.filter_snapshot(&id, &mut snapshot, request)?;
            Ok(GpuDeviceSnapshot {
                device_id: id,
                snapshot,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(GpuMonitorSnapshot {
        sampled_at: crate::model::now_millis(),
        gpus,
    })
}

fn deliver_due(
    monitor: &MonitorInner,
    subscriptions: &mut HashMap<u64, SubscriptionEntry>,
    pending: &mut Vec<PendingSample>,
    cache: &mut HashMap<String, CachedSnapshot>,
) {
    let now = Instant::now();
    let inventory = monitor.device_ids();
    let mut demands: HashMap<String, (bool, bool)> = HashMap::new();
    let due: Vec<_> = subscriptions
        .iter()
        .filter_map(|(id, entry)| (entry.next_due <= now).then_some(*id))
        .collect();
    for id in &due {
        let entry = &subscriptions[id];
        for device in target_ids(&entry.device_id, &inventory) {
            demands.entry(device).or_default().0 |= entry.options.include_processes;
        }
    }
    for entry in pending
        .iter()
        .filter(|entry| entry.ready_at.is_none_or(|due| due <= now))
    {
        for device in target_ids(&entry.device_id, &inventory) {
            let demand = demands.entry(device).or_default();
            demand.0 |= entry.request.include_processes
                && entry.request.wants(crate::model::MetricKey::Processes);
            demand.1 |= entry.ready_at.is_some();
        }
    }
    cache.retain(|id, value| {
        inventory.contains(id)
            && value.collected_at.elapsed() <= Duration::from_millis(SNAPSHOT_COALESCE_MS)
    });
    let missing: Vec<_> = demands
        .iter()
        .filter_map(|(id, (_, force))| {
            (!cache.contains_key(id)
                || (*force
                    && cache
                        .get(id)
                        .is_some_and(|value| snapshot_has_first_sample(&value.snapshot))))
            .then_some(id.clone())
        })
        .collect();
    let mut values = HashMap::new();
    if !missing.is_empty() {
        for (id, result) in monitor.sample_many(
            &missing,
            &SampleRequest {
                window_ms: 0,
                metrics: None,
                include_processes: false,
            },
        ) {
            match result {
                Ok(snapshot) => {
                    cache.insert(
                        id,
                        CachedSnapshot {
                            collected_at: Instant::now(),
                            snapshot,
                            processes_collected: false,
                        },
                    );
                }
                Err(error) => {
                    values.insert(id, Err(error));
                }
            }
        }
    }
    for (id, (include_processes, _)) in demands {
        if let Some(value) = cache.get_mut(&id) {
            if include_processes && !value.processes_collected {
                match monitor.process_snapshot(&id) {
                    Ok(processes) => value.snapshot.processes = processes,
                    Err(error) => {
                        values.insert(id, Err(error));
                        continue;
                    }
                }
                value.processes_collected = true;
            }
            values.insert(id, Ok(value.snapshot.clone()));
        }
    }
    for id in due {
        if let Some(entry) = subscriptions.get_mut(&id) {
            match batch_for(
                monitor,
                &entry.device_id,
                &inventory,
                &SampleRequest {
                    window_ms: 0,
                    metrics: None,
                    include_processes: entry.options.include_processes,
                },
                &values,
            ) {
                Ok(snapshot) => entry.slot.publish(snapshot),
                Err(error) => entry.slot.fail(error.to_string()),
            }
            let completed = Instant::now();
            let interval = Duration::from_millis(entry.options.interval_ms);
            while entry.next_due <= completed {
                entry.next_due += interval;
            }
        }
    }
    let mut retained = Vec::new();
    for mut entry in pending.drain(..) {
        if entry.ready_at.is_some_and(|due| due > now) {
            retained.push(entry);
            continue;
        }
        let result = batch_for(
            monitor,
            &entry.device_id,
            &inventory,
            &entry.request,
            &values,
        );
        if entry.ready_at.is_none()
            && entry.request.window_ms > 0
            && result.as_ref().is_ok_and(|batch| {
                batch
                    .gpus
                    .iter()
                    .any(|gpu| snapshot_has_first_sample(&gpu.snapshot))
            })
        {
            entry.ready_at = Some(Instant::now() + Duration::from_millis(entry.request.window_ms));
            retained.push(entry);
        } else {
            let _ = entry.response.send(result);
        }
    }
    *pending = retained;
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

    fn snapshot(sampled_at: u64) -> GpuMonitorSnapshot {
        let value: GpuSnapshot = serde_json::from_value(serde_json::json!({
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
        .expect("valid test snapshot");
        GpuMonitorSnapshot {
            sampled_at,
            gpus: vec![GpuDeviceSnapshot {
                device_id: "test".into(),
                snapshot: value,
            }],
        }
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
