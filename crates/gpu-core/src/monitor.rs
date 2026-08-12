use crate::correlation::correlate;
use crate::error::{GpuError, Result};
use crate::merge::merge_metrics;
use crate::model::{
    CanonicalGpu, DeviceMergeDiagnostics, Diagnostics, GpuProcessSnapshot, MetricMergeDiagnostic,
    ProviderDiagnostic, ProviderId, ProviderSample, SampleRequest, UnavailableObservation,
    UnavailableReason, now_millis,
};
use crate::provider::{Provider, ProviderMetadata};
use crate::providers;
use crate::sampler::{SampleSubscription, SamplerHub, WatchOptions};
use crate::snapshot::{GpuSnapshot, build_snapshot};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

const MAX_PROVIDERS: usize = 32;
const MAX_REQUIRED_PROVIDERS: usize = 16;
const MAX_PROVIDER_ID_BYTES: usize = 64;
const MAX_DEVICE_ID_BYTES: usize = 512;
const MAX_PROVIDER_OBSERVATIONS: usize = 512;
const MAX_TOTAL_OBSERVATIONS: usize = 1_024;
const MAX_PROVIDER_SAMPLE_VALUES: usize = 128;
const MAX_PROCESSES_PER_SNAPSHOT: usize = 16_384;
const MAX_DIAGNOSTIC_WARNINGS: usize = 128;
const MAX_DIAGNOSTIC_MESSAGE_BYTES: usize = 2_048;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MonitorOptions {
    #[serde(default, alias = "requiredProvider")]
    pub required_providers: Vec<String>,
    #[serde(default = "default_true")]
    pub enable_apple_private_telemetry: bool,
    #[serde(default)]
    pub include_software_adapters: bool,
}

const fn default_true() -> bool {
    true
}

impl Default for MonitorOptions {
    fn default() -> Self {
        Self {
            required_providers: Vec::new(),
            enable_apple_private_telemetry: true,
            include_software_adapters: false,
        }
    }
}

pub struct GpuMonitor {
    inner: Arc<MonitorInner>,
}

pub(crate) struct MonitorInner {
    providers: Vec<Arc<dyn Provider>>,
    devices: RwLock<Vec<CanonicalGpu>>,
    warnings: RwLock<Vec<String>>,
    merge_diagnostics: RwLock<BTreeMap<String, Vec<MetricMergeDiagnostic>>>,
    sampler: OnceLock<SamplerHub>,
    closed: AtomicBool,
    external_handles: AtomicUsize,
}

impl GpuMonitor {
    pub fn open(options: MonitorOptions) -> Result<Self> {
        validate_monitor_options(&options)?;
        let providers = providers::default_providers(&options);
        Self::with_providers_and_options(providers, options)
    }

    pub fn with_providers(providers: Vec<Arc<dyn Provider>>) -> Result<Self> {
        Self::with_providers_and_options(providers, MonitorOptions::default())
    }

    fn with_providers_and_options(
        providers: Vec<Arc<dyn Provider>>,
        options: MonitorOptions,
    ) -> Result<Self> {
        validate_monitor_options(&options)?;
        if providers.len() > MAX_PROVIDERS {
            return Err(GpuError::InvalidArgument(format!(
                "provider count exceeds the safety limit ({MAX_PROVIDERS})"
            )));
        }
        let inner = Arc::new(MonitorInner {
            providers,
            devices: RwLock::new(Vec::new()),
            warnings: RwLock::new(Vec::new()),
            merge_diagnostics: RwLock::new(BTreeMap::new()),
            sampler: OnceLock::new(),
            closed: AtomicBool::new(false),
            external_handles: AtomicUsize::new(1),
        });
        inner.refresh_devices()?;

        for required in options.required_providers {
            let diagnostic = inner
                .providers
                .iter()
                .find(|provider| provider.provider_id() == required)
                .map(|provider| provider.diagnostic());
            match diagnostic {
                Some(value) if value.loaded => {}
                Some(value) => {
                    return Err(GpuError::provider(
                        required,
                        value.reason.unwrap_or(UnavailableReason::ProviderError),
                        value
                            .message
                            .unwrap_or_else(|| "required provider did not initialize".into()),
                    ));
                }
                None => {
                    return Err(GpuError::InvalidArgument(format!(
                        "unknown required provider: {required}"
                    )));
                }
            }
        }

        // The sampler owns one strong monitor reference so a JavaScript GC
        // finalizer can request nonblocking shutdown without dropping driver
        // objects on the finalizer thread. `external_handles` deliberately
        // excludes this internal owner.
        let sampler = SamplerHub::start(Arc::clone(&inner))?;
        inner
            .sampler
            .set(sampler)
            .map_err(|_| GpuError::Internal("sampler initialized twice".into()))?;
        Ok(Self { inner })
    }

    pub fn gpus(&self) -> Result<Vec<CanonicalGpu>> {
        self.ensure_open()?;
        Ok(self.inner.devices.read().clone())
    }

    pub fn sample(
        &self,
        device_id: impl Into<String>,
        request: SampleRequest,
    ) -> Result<GpuSnapshot> {
        self.ensure_open()?;
        if request.window_ms > 60_000 {
            return Err(GpuError::InvalidArgument(
                "windowMs must be between 0 and 60000".into(),
            ));
        }
        let device_id = device_id.into();
        validate_device_id(&device_id)?;
        self.sampler()?.sample(device_id, request)
    }

    pub fn samples(
        &self,
        device_id: impl Into<String>,
        options: WatchOptions,
    ) -> Result<SampleSubscription> {
        self.ensure_open()?;
        let device_id = device_id.into();
        validate_device_id(&device_id)?;
        if !self
            .inner
            .devices
            .read()
            .iter()
            .any(|gpu| gpu.identity.id == device_id)
        {
            return Err(GpuError::DeviceNotFound(device_id));
        }
        self.sampler()?.subscribe(device_id, options)
    }

    pub fn refresh(&self) -> Result<Vec<CanonicalGpu>> {
        self.ensure_open()?;
        self.sampler()?.refresh()?;
        self.gpus()
    }

    pub fn diagnostics(&self) -> Diagnostics {
        self.inner.diagnostics()
    }

    pub fn vendor_info(&self, device_id: &str) -> Result<serde_json::Value> {
        self.ensure_open()?;
        validate_device_id(device_id)?;
        let gpu = self
            .inner
            .devices
            .read()
            .iter()
            .find(|gpu| gpu.identity.id == device_id)
            .cloned()
            .ok_or_else(|| GpuError::DeviceNotFound(device_id.into()))?;
        let mut result = serde_json::Map::new();
        for (key, value) in &gpu.vendor_info {
            if key.len() <= 256 && bounded_json_value(value) {
                result.insert(key.clone(), value.clone());
            } else {
                self.inner.record_warning(
                    "inventory vendor info exceeded structural safety limits".into(),
                );
            }
        }
        for provider in &self.inner.providers {
            match provider.vendor_info(&gpu) {
                Ok(serde_json::Value::Object(values)) if bounded_json_object(&values) => {
                    result.extend(values);
                }
                Ok(serde_json::Value::Object(_)) => self.inner.record_warning(format!(
                    "{} vendor info exceeded structural safety limits",
                    provider.provider_id()
                )),
                Ok(serde_json::Value::Null) => {}
                Ok(value) if bounded_json_value(&value) => {
                    result.insert(provider.provider_id().into(), value);
                }
                Ok(_) => self.inner.record_warning(format!(
                    "{} vendor info exceeded structural safety limits",
                    provider.provider_id()
                )),
                Err(error) => self.inner.record_warning(format!(
                    "{} vendor info failed: {error}",
                    provider.provider_id()
                )),
            }
        }
        Ok(serde_json::Value::Object(result))
    }

    pub fn close(&self) {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(sampler) = self.inner.sampler.get() {
            sampler.shutdown();
        } else {
            self.inner.shutdown_providers();
        }
    }

    /// Requests cancellation without waiting for the sampler thread. This is
    /// intended for foreign-runtime finalizers; ordinary callers should use
    /// [`Self::close`] for deterministic cleanup.
    #[doc(hidden)]
    pub fn request_close_nonblocking(&self) {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(sampler) = self.inner.sampler.get() {
            sampler.request_shutdown_nonblocking();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    fn ensure_open(&self) -> Result<()> {
        if self.is_closed() {
            Err(GpuError::MonitorClosed)
        } else {
            Ok(())
        }
    }

    fn sampler(&self) -> Result<&SamplerHub> {
        self.inner
            .sampler
            .get()
            .ok_or_else(|| GpuError::Internal("monitor sampler was not initialized".into()))
    }
}

impl Clone for GpuMonitor {
    fn clone(&self) -> Self {
        self.inner.external_handles.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Drop for GpuMonitor {
    fn drop(&mut self) {
        // Clones back short-lived async tasks. Only the last external handle
        // requests implicit shutdown; the sampler's internal Arc keeps
        // providers alive until shutdown runs on the sampler thread.
        if self.inner.external_handles.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.request_close_nonblocking();
        }
    }
}

impl MonitorInner {
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub(crate) fn refresh_devices(&self) -> Result<()> {
        if self.is_closed() {
            return Err(GpuError::MonitorClosed);
        }
        let mut observations = Vec::new();
        for provider in &self.providers {
            match provider.enumerate() {
                Ok(mut provider_observations) => {
                    if provider_observations.len() > MAX_PROVIDER_OBSERVATIONS {
                        self.record_warning(format!(
                            "{} returned {} inventory observations; truncating to {MAX_PROVIDER_OBSERVATIONS}",
                            provider.provider_id(),
                            provider_observations.len()
                        ));
                        provider_observations.truncate(MAX_PROVIDER_OBSERVATIONS);
                    }
                    let remaining = MAX_TOTAL_OBSERVATIONS.saturating_sub(observations.len());
                    if provider_observations.len() > remaining {
                        self.record_warning(format!(
                            "inventory reached the {MAX_TOTAL_OBSERVATIONS}-observation safety limit"
                        ));
                        provider_observations.truncate(remaining);
                    }
                    observations.append(&mut provider_observations);
                    if observations.len() >= MAX_TOTAL_OBSERVATIONS {
                        break;
                    }
                }
                Err(error) => self.record_warning(format!(
                    "{} inventory failed: {error}",
                    provider.provider_id()
                )),
            }
        }

        let mut devices = correlate(observations);
        for device in &mut devices {
            for provider in &self.providers {
                device.capability_set.extend(&provider.capabilities(device));
            }
            device.capabilities = device.capability_set.to_public();
        }
        *self.devices.write() = devices;
        let current_ids: std::collections::BTreeSet<_> = self
            .devices
            .read()
            .iter()
            .map(|device| device.identity.id.clone())
            .collect();
        self.merge_diagnostics
            .write()
            .retain(|device_id, _| current_ids.contains(device_id));
        Ok(())
    }

    pub(crate) fn sample_once(
        &self,
        device_id: &str,
        request: &SampleRequest,
    ) -> Result<GpuSnapshot> {
        if self.is_closed() {
            return Err(GpuError::MonitorClosed);
        }
        let gpu = self
            .devices
            .read()
            .iter()
            .find(|gpu| gpu.identity.id == device_id)
            .cloned()
            .ok_or_else(|| GpuError::DeviceNotFound(device_id.into()))?;

        let mut metrics = Vec::new();
        let mut unavailable = Vec::new();
        let mut best_processes: Option<(i16, Vec<GpuProcessSnapshot>)> = None;
        let mut metadata: BTreeMap<ProviderId, ProviderMetadata> = BTreeMap::new();

        for provider in &self.providers {
            let provider_metadata = provider.metadata();
            metadata.insert(provider_metadata.id.to_owned(), provider_metadata.clone());
            let capabilities = provider.capabilities(&gpu);
            match provider.sample(&gpu, request) {
                Ok(sample) => {
                    let mut sample = sample;
                    if sample.metrics.len() > MAX_PROVIDER_SAMPLE_VALUES {
                        self.record_warning(format!(
                            "{} returned {} metric values; truncating to {MAX_PROVIDER_SAMPLE_VALUES}",
                            provider.provider_id(),
                            sample.metrics.len()
                        ));
                        sample.metrics.truncate(MAX_PROVIDER_SAMPLE_VALUES);
                    }
                    if sample.unavailable.len() > MAX_PROVIDER_SAMPLE_VALUES {
                        self.record_warning(format!(
                            "{} returned {} unavailable values; truncating to {MAX_PROVIDER_SAMPLE_VALUES}",
                            provider.provider_id(),
                            sample.unavailable.len()
                        ));
                        sample.unavailable.truncate(MAX_PROVIDER_SAMPLE_VALUES);
                    }
                    if let Some(processes) = &mut sample.processes
                        && processes.len() > MAX_PROCESSES_PER_SNAPSHOT
                    {
                        self.record_warning(format!(
                            "{} returned {} process records; truncating to {MAX_PROCESSES_PER_SNAPSHOT}",
                            provider.provider_id(),
                            processes.len()
                        ));
                        processes.truncate(MAX_PROCESSES_PER_SNAPSHOT);
                    }
                    collect_provider_sample(
                        &gpu,
                        provider_metadata.specificity,
                        sample,
                        &mut metrics,
                        &mut unavailable,
                        &mut best_processes,
                    );
                }
                Err(error) => {
                    let (reason, message) = provider_error_details(&error);
                    unavailable.extend(
                        capabilities
                            .metrics()
                            .filter(|metric| request.wants(*metric))
                            .map(|metric| UnavailableObservation {
                                device_id: gpu.identity.id.clone(),
                                metric,
                                reason,
                                source: Some(provider.provider_id().into()),
                                message: Some(message.clone()),
                            }),
                    );
                    self.record_warning(format!(
                        "{} sample failed for {}: {error}",
                        provider.provider_id(),
                        gpu.identity.id
                    ));
                }
            }
        }

        let sampled_at = now_millis();
        let merged = merge_metrics(metrics, unavailable, &metadata, sampled_at);
        self.merge_diagnostics
            .write()
            .insert(gpu.identity.id.clone(), merged.diagnostics.clone());
        let processes = request
            .include_processes
            .then(|| best_processes.map(|(_, values)| values))
            .flatten();
        Ok(build_snapshot(&gpu, &merged, sampled_at, processes))
    }

    pub(crate) fn shutdown_providers(&self) {
        for provider in &self.providers {
            provider.shutdown();
        }
    }

    fn record_warning(&self, warning: String) {
        let warning = bounded_diagnostic_message(warning);
        let mut warnings = self.warnings.write();
        if !warnings.contains(&warning) {
            if warnings.len() < MAX_DIAGNOSTIC_WARNINGS.saturating_sub(1) {
                warnings.push(warning);
            } else if warnings.len() < MAX_DIAGNOSTIC_WARNINGS {
                warnings.push(format!(
                    "additional warnings omitted after reaching the {MAX_DIAGNOSTIC_WARNINGS}-warning safety limit"
                ));
            }
        }
    }

    fn diagnostics(&self) -> Diagnostics {
        let devices = self.devices.read();
        let providers = self
            .providers
            .iter()
            .map(|provider| {
                let mut diagnostic: ProviderDiagnostic = provider.diagnostic();
                diagnostic.message = diagnostic.message.map(bounded_diagnostic_message);
                diagnostic.devices_matched = devices
                    .iter()
                    .filter(|gpu| {
                        gpu.provider_device_ids.contains_key(provider.provider_id())
                            || provider.capabilities(gpu).metrics().next().is_some()
                    })
                    .count();
                diagnostic
            })
            .collect();
        let metric_selections = self
            .merge_diagnostics
            .read()
            .iter()
            .map(|(device_id, metrics)| DeviceMergeDiagnostics {
                device_id: device_id.clone(),
                metrics: metrics.clone(),
            })
            .collect();
        Diagnostics {
            platform: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            providers,
            warnings: self.warnings.read().clone(),
            metric_selections,
        }
    }
}

fn validate_monitor_options(options: &MonitorOptions) -> Result<()> {
    if options.required_providers.len() > MAX_REQUIRED_PROVIDERS {
        return Err(GpuError::InvalidArgument(format!(
            "requiredProviders exceeds the {MAX_REQUIRED_PROVIDERS}-entry safety limit"
        )));
    }
    for provider in &options.required_providers {
        if provider.is_empty()
            || provider.len() > MAX_PROVIDER_ID_BYTES
            || !provider.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.')
            })
        {
            return Err(GpuError::InvalidArgument(
                "requiredProviders contains an invalid provider identifier".into(),
            ));
        }
    }
    Ok(())
}

fn validate_device_id(device_id: &str) -> Result<()> {
    if device_id.is_empty()
        || device_id.len() > MAX_DEVICE_ID_BYTES
        || device_id.chars().any(char::is_control)
    {
        return Err(GpuError::InvalidArgument(
            "device id is empty, oversized, or contains control characters".into(),
        ));
    }
    Ok(())
}

fn bounded_diagnostic_message(mut message: String) -> String {
    if message.len() <= MAX_DIAGNOSTIC_MESSAGE_BYTES {
        return message;
    }
    let mut end = MAX_DIAGNOSTIC_MESSAGE_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message.push_str("...");
    message
}

fn bounded_json_object(values: &serde_json::Map<String, serde_json::Value>) -> bool {
    const MAX_NODES: usize = 65_536;
    const MAX_COLLECTION_ENTRIES: usize = 16_384;
    const MAX_KEY_BYTES: usize = 256;

    if values.len() > MAX_COLLECTION_ENTRIES || values.keys().any(|key| key.len() > MAX_KEY_BYTES) {
        return false;
    }
    bounded_json_stack(
        values.values().map(|value| (value, 1_usize)).collect(),
        MAX_NODES.saturating_sub(1),
    )
}

fn bounded_json_value(value: &serde_json::Value) -> bool {
    const MAX_NODES: usize = 65_536;
    bounded_json_stack(vec![(value, 0_usize)], MAX_NODES)
}

fn bounded_json_stack(mut stack: Vec<(&serde_json::Value, usize)>, mut remaining: usize) -> bool {
    const MAX_DEPTH: usize = 16;
    const MAX_COLLECTION_ENTRIES: usize = 16_384;
    const MAX_STRING_BYTES: usize = 65_536;
    const MAX_KEY_BYTES: usize = 256;

    while let Some((value, depth)) = stack.pop() {
        if remaining == 0 || depth > MAX_DEPTH {
            return false;
        }
        remaining -= 1;
        match value {
            serde_json::Value::String(value) => {
                if value.len() > MAX_STRING_BYTES {
                    return false;
                }
            }
            serde_json::Value::Array(values) => {
                if values.len() > MAX_COLLECTION_ENTRIES || values.len() > remaining {
                    return false;
                }
                stack.extend(values.iter().map(|value| (value, depth + 1)));
            }
            serde_json::Value::Object(values) => {
                if values.len() > MAX_COLLECTION_ENTRIES
                    || values.len() > remaining
                    || values.keys().any(|key| key.len() > MAX_KEY_BYTES)
                {
                    return false;
                }
                stack.extend(values.values().map(|value| (value, depth + 1)));
            }
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            }
        }
    }
    true
}

fn collect_provider_sample(
    gpu: &CanonicalGpu,
    specificity: i16,
    mut sample: ProviderSample,
    metrics: &mut Vec<crate::model::MetricObservation>,
    unavailable: &mut Vec<UnavailableObservation>,
    best_processes: &mut Option<(i16, Vec<GpuProcessSnapshot>)>,
) {
    for metric in &mut sample.metrics {
        metric.device_id.clone_from(&gpu.identity.id);
    }
    for value in &mut sample.unavailable {
        value.device_id.clone_from(&gpu.identity.id);
    }
    metrics.append(&mut sample.metrics);
    unavailable.append(&mut sample.unavailable);
    if let Some(processes) = sample.processes.take()
        && best_processes
            .as_ref()
            .is_none_or(|(score, _)| specificity > *score)
    {
        *best_processes = Some((specificity, processes));
    }
}

fn provider_error_details(error: &GpuError) -> (UnavailableReason, String) {
    match error {
        GpuError::Provider {
            reason, message, ..
        } => (*reason, message.clone()),
        GpuError::DeviceNotFound(_) => (UnavailableReason::DeviceLost, error.to_string()),
        _ => (UnavailableReason::ProviderError, error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CapabilitySet, DeviceObservation, GpuVendor, Metric, MetricKey, MetricObservation,
        MetricQuality, MetricValue,
    };
    use crate::provider::ProviderMetadata;
    use crate::providers::mock::MockProvider;
    use crate::providers::optional_runtime::UnavailableProvider;
    use crate::sampler::WatchOptions;
    use std::future::Future;
    use std::sync::mpsc;
    use std::task::{Context, Poll, Wake, Waker};
    use std::thread;
    use std::time::Duration;

    struct TestWake(mpsc::SyncSender<()>);

    impl Wake for TestWake {
        fn wake(self: Arc<Self>) {
            let _ = self.0.try_send(());
        }

        fn wake_by_ref(self: &Arc<Self>) {
            let _ = self.0.try_send(());
        }
    }

    fn wait_next(subscription: &SampleSubscription) -> Result<Option<GpuSnapshot>> {
        let mut future = Box::pin(subscription.next_async()?);
        let (sender, receiver) = mpsc::sync_channel(1);
        let waker = Waker::from(Arc::new(TestWake(sender)));
        let mut context = Context::from_waker(&waker);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => receiver
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|_| GpuError::Internal("test subscription timed out".into()))?,
            }
        }
    }

    fn mock_monitor(samples: Vec<ProviderSample>) -> (GpuMonitor, Arc<MockProvider>) {
        let mut observation = DeviceObservation::new("mock", "zero", GpuVendor::Intel, "Mock GPU");
        observation.uuid = Some("mock-uuid".into());
        observation.capabilities = CapabilitySet::new([MetricKey::UtilizationOverall]);
        let provider = Arc::new(MockProvider::new(
            ProviderMetadata::new("mock", 100, 100),
            vec![observation],
            samples,
        ));
        let monitor = GpuMonitor::with_providers(vec![Arc::clone(&provider) as Arc<dyn Provider>])
            .expect("mock monitor should open");
        (monitor, provider)
    }

    #[test]
    fn missing_metric_is_not_zero() {
        let (monitor, _) = mock_monitor(vec![ProviderSample::default()]);
        let id = monitor.gpus().unwrap()[0].identity.id.clone();
        let snapshot = monitor
            .sample(
                id,
                SampleRequest {
                    window_ms: 0,
                    ..SampleRequest::default()
                },
            )
            .unwrap();
        assert!(matches!(
            snapshot.utilization.overall,
            Metric::Unavailable(_)
        ));
        monitor.close();
    }

    #[test]
    fn an_unsupported_process_request_is_not_an_empty_process_list() {
        let (monitor, _) = mock_monitor(vec![ProviderSample::default()]);
        let id = monitor.gpus().unwrap()[0].identity.id.clone();
        let snapshot = monitor
            .sample(
                id,
                SampleRequest {
                    window_ms: 0,
                    include_processes: true,
                    ..SampleRequest::default()
                },
            )
            .unwrap();
        assert!(snapshot.processes.is_none());
        monitor.close();
    }

    #[test]
    fn first_sample_is_retried_for_requested_window() {
        let unavailable = ProviderSample {
            unavailable: vec![UnavailableObservation {
                device_id: String::new(),
                metric: MetricKey::UtilizationOverall,
                reason: UnavailableReason::FirstSample,
                source: Some("mock".into()),
                message: None,
            }],
            ..ProviderSample::default()
        };
        let available = ProviderSample {
            metrics: vec![MetricObservation {
                device_id: String::new(),
                metric: MetricKey::UtilizationOverall,
                value: MetricValue::Number(40.0),
                source: "mock".into(),
                quality: MetricQuality::Derived,
                sampled_at: now_millis(),
                interval_ms: Some(1),
                definition: Some("mock busy delta".into()),
            }],
            ..ProviderSample::default()
        };
        let (monitor, _) = mock_monitor(vec![unavailable, available]);
        let id = monitor.gpus().unwrap()[0].identity.id.clone();
        let snapshot = monitor
            .sample(
                id,
                SampleRequest {
                    window_ms: 1,
                    ..SampleRequest::default()
                },
            )
            .unwrap();
        assert!(snapshot.utilization.overall.is_available());
        let diagnostics = monitor.diagnostics();
        assert_eq!(diagnostics.metric_selections.len(), 1);
        assert_eq!(diagnostics.metric_selections[0].metrics.len(), 1);
        assert!(
            diagnostics.metric_selections[0].metrics[0]
                .candidates
                .iter()
                .any(|candidate| candidate.selected && candidate.source == "mock")
        );
        monitor.close();
    }

    #[test]
    fn close_is_idempotent_and_shuts_down_providers() {
        let (monitor, provider) = mock_monitor(vec![]);
        monitor.close();
        monitor.close();
        assert!(provider.was_shutdown());
        assert!(matches!(monitor.gpus(), Err(GpuError::MonitorClosed)));
    }

    #[test]
    fn dropping_the_last_handle_requests_sampler_owned_shutdown() {
        let (monitor, provider) = mock_monitor(vec![]);
        drop(monitor);
        for _ in 0..100 {
            if provider.was_shutdown() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(provider.was_shutdown());
    }

    #[test]
    fn nonblocking_close_wakes_waiters_while_a_clone_is_alive() {
        let (monitor, provider) = mock_monitor(vec![ProviderSample::default()]);
        let task_clone = monitor.clone();
        let id = monitor.gpus().unwrap()[0].identity.id.clone();
        let subscription = Arc::new(monitor.samples(id, WatchOptions::default()).unwrap());
        assert!(wait_next(&subscription).unwrap().is_some());
        let waiting = Arc::clone(&subscription);
        let waiter = thread::spawn(move || wait_next(&waiting));
        thread::sleep(Duration::from_millis(10));

        monitor.request_close_nonblocking();
        assert!(waiter.join().unwrap().unwrap().is_none());
        assert!(matches!(task_clone.gpus(), Err(GpuError::MonitorClosed)));
        drop(task_clone);
        for _ in 0..100 {
            if provider.was_shutdown() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(provider.was_shutdown());
    }

    #[test]
    fn a_missing_optional_runtime_does_not_block_initialization() {
        let provider = Arc::new(UnavailableProvider::new(
            "optional-vendor",
            UnavailableReason::DriverLibraryMissing,
            "fixture runtime is absent",
        ));
        let monitor = GpuMonitor::with_providers(vec![provider]).unwrap();
        assert!(monitor.gpus().unwrap().is_empty());
        assert!(!monitor.diagnostics().providers[0].loaded);
        monitor.close();
    }

    #[test]
    fn cancelling_a_subscription_wakes_a_pending_consumer() {
        let (monitor, _) = mock_monitor(vec![ProviderSample::default()]);
        let id = monitor.gpus().unwrap()[0].identity.id.clone();
        let subscription = Arc::new(
            monitor
                .samples(
                    id,
                    WatchOptions {
                        interval_ms: 1_000,
                        include_processes: false,
                    },
                )
                .unwrap(),
        );
        assert!(wait_next(&subscription).unwrap().is_some());

        let waiting = Arc::clone(&subscription);
        let waiter = thread::spawn(move || wait_next(&waiting));
        thread::sleep(Duration::from_millis(10));
        subscription.cancel();
        assert!(waiter.join().unwrap().unwrap().is_none());
        monitor.close();
    }

    #[test]
    fn nearby_listeners_share_a_native_poll() {
        let (monitor, provider) = mock_monitor(vec![ProviderSample::default()]);
        let id = monitor.gpus().unwrap()[0].identity.id.clone();
        let first = monitor
            .samples(id.clone(), WatchOptions::default())
            .unwrap();
        let second = monitor.samples(id, WatchOptions::default()).unwrap();

        assert!(wait_next(&first).unwrap().is_some());
        assert!(wait_next(&second).unwrap().is_some());
        assert_eq!(provider.sample_count(), 1);
        first.cancel();
        second.cancel();
        monitor.close();
    }

    #[test]
    fn monitor_close_wakes_a_pending_subscription() {
        let (monitor, _) = mock_monitor(vec![ProviderSample::default()]);
        let id = monitor.gpus().unwrap()[0].identity.id.clone();
        let subscription = Arc::new(monitor.samples(id, WatchOptions::default()).unwrap());
        assert!(wait_next(&subscription).unwrap().is_some());

        let waiting = Arc::clone(&subscription);
        let waiter = thread::spawn(move || wait_next(&waiting));
        thread::sleep(Duration::from_millis(10));
        monitor.close();
        assert!(waiter.join().unwrap().unwrap().is_none());
    }

    #[test]
    fn native_subscription_count_is_bounded() {
        let (monitor, _) = mock_monitor(vec![ProviderSample::default()]);
        let id = monitor.gpus().unwrap()[0].identity.id.clone();
        let mut subscriptions = Vec::new();
        for _ in 0..crate::sampler::MAX_SUBSCRIPTIONS {
            subscriptions.push(
                monitor
                    .samples(id.clone(), WatchOptions::default())
                    .unwrap(),
            );
        }
        assert!(matches!(
            monitor.samples(id, WatchOptions::default()),
            Err(GpuError::Backpressure(_))
        ));
        for subscription in subscriptions {
            subscription.cancel();
        }
        monitor.close();
    }

    #[test]
    fn monitor_input_and_diagnostic_growth_are_bounded() {
        let options = MonitorOptions {
            required_providers: (0..=MAX_REQUIRED_PROVIDERS)
                .map(|index| format!("provider-{index}"))
                .collect(),
            ..MonitorOptions::default()
        };
        assert!(matches!(
            GpuMonitor::open(options),
            Err(GpuError::InvalidArgument(_))
        ));
        assert!(validate_device_id("").is_err());
        assert!(validate_device_id(&"x".repeat(MAX_DEVICE_ID_BYTES + 1)).is_err());

        let (monitor, _) = mock_monitor(vec![]);
        for index in 0..(MAX_DIAGNOSTIC_WARNINGS + 10) {
            monitor.inner.record_warning(format!("warning {index}"));
        }
        let diagnostics = monitor.diagnostics();
        assert_eq!(diagnostics.warnings.len(), MAX_DIAGNOSTIC_WARNINGS);
        assert!(
            diagnostics
                .warnings
                .last()
                .is_some_and(|warning| warning.contains("additional warnings omitted"))
        );
        monitor.close();
    }

    #[test]
    fn provider_process_results_and_json_are_bounded() {
        let processes = (0..=MAX_PROCESSES_PER_SNAPSHOT)
            .map(|pid| GpuProcessSnapshot {
                pid: u32::try_from(pid).unwrap_or(u32::MAX),
                name: None,
                memory_used_bytes: None,
                utilization: None,
            })
            .collect();
        let (monitor, _) = mock_monitor(vec![ProviderSample {
            processes: Some(processes),
            ..ProviderSample::default()
        }]);
        let id = monitor.gpus().unwrap()[0].identity.id.clone();
        let snapshot = monitor
            .sample(
                id,
                SampleRequest {
                    window_ms: 0,
                    include_processes: true,
                    ..SampleRequest::default()
                },
            )
            .unwrap();
        assert_eq!(
            snapshot.processes.as_ref().map(Vec::len),
            Some(MAX_PROCESSES_PER_SNAPSHOT)
        );
        assert!(
            monitor
                .diagnostics()
                .warnings
                .iter()
                .any(|warning| warning.contains("process records"))
        );
        monitor.close();

        let mut deeply_nested = serde_json::Value::Null;
        for _ in 0..=17 {
            deeply_nested = serde_json::json!([deeply_nested]);
        }
        assert!(!bounded_json_value(&deeply_nested));
        assert!(!bounded_json_value(&serde_json::Value::String(
            "x".repeat(65_537)
        )));
    }
}
