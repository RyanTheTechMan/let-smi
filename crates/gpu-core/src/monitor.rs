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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
}

impl GpuMonitor {
    pub fn open(options: MonitorOptions) -> Result<Self> {
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
        let inner = Arc::new(MonitorInner {
            providers,
            devices: RwLock::new(Vec::new()),
            warnings: RwLock::new(Vec::new()),
            merge_diagnostics: RwLock::new(BTreeMap::new()),
            sampler: OnceLock::new(),
            closed: AtomicBool::new(false),
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

        let sampler = SamplerHub::start(Arc::downgrade(&inner))?;
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
        self.sampler()?.sample(device_id.into(), request)
    }

    pub fn samples(
        &self,
        device_id: impl Into<String>,
        options: WatchOptions,
    ) -> Result<SampleSubscription> {
        self.ensure_open()?;
        let device_id = device_id.into();
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
            result.insert(key.clone(), value.clone());
        }
        for provider in &self.inner.providers {
            match provider.vendor_info(&gpu) {
                Ok(serde_json::Value::Object(values)) => result.extend(values),
                Ok(serde_json::Value::Null) => {}
                Ok(value) => {
                    result.insert(provider.provider_id().into(), value);
                }
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
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Drop for GpuMonitor {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) == 1 {
            self.close();
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
                    observations.append(&mut provider_observations);
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
            .then(|| best_processes.map_or_else(Vec::new, |(_, values)| values));
        Ok(build_snapshot(&gpu, &merged, sampled_at, processes))
    }

    pub(crate) fn shutdown_providers(&self) {
        for provider in &self.providers {
            provider.shutdown();
        }
    }

    fn record_warning(&self, warning: String) {
        let mut warnings = self.warnings.write();
        if !warnings.contains(&warning) {
            warnings.push(warning);
        }
    }

    fn diagnostics(&self) -> Diagnostics {
        let devices = self.devices.read();
        let providers = self
            .providers
            .iter()
            .map(|provider| {
                let mut diagnostic: ProviderDiagnostic = provider.diagnostic();
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
    if !sample.processes.is_empty()
        && best_processes
            .as_ref()
            .is_none_or(|(score, _)| specificity > *score)
    {
        *best_processes = Some((specificity, sample.processes));
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
    use std::thread;
    use std::time::Duration;

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
        assert!(subscription.next().unwrap().is_some());

        let waiting = Arc::clone(&subscription);
        let waiter = thread::spawn(move || waiting.next());
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

        assert!(first.next().unwrap().is_some());
        assert!(second.next().unwrap().is_some());
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
        assert!(subscription.next().unwrap().is_some());

        let waiting = Arc::clone(&subscription);
        let waiter = thread::spawn(move || waiting.next());
        thread::sleep(Duration::from_millis(10));
        monitor.close();
        assert!(waiter.join().unwrap().unwrap().is_none());
    }
}
