use super::inventory::{LinuxDeviceRecord, PROVIDER_ID, discover, vendor_info};
use super::metric::{MetricReadError, select_error};
use super::roots::LinuxRoots;
use crate::error::Result;
use crate::model::{
    CanonicalGpu, CapabilitySet, DeviceObservation, MetricKey, MetricObservation,
    ProviderDiagnostic, ProviderSample, SampleRequest, UnavailableObservation, UnavailableReason,
    now_millis,
};
use crate::provider::{InventoryProvider, ProviderMetadata, TelemetryProvider};
use parking_lot::RwLock;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
struct DiagnosticState {
    loaded: bool,
    devices_matched: usize,
    reason: Option<UnavailableReason>,
    message: Option<String>,
}

impl Default for DiagnosticState {
    fn default() -> Self {
        Self {
            loaded: true,
            devices_matched: 0,
            reason: None,
            message: None,
        }
    }
}

pub struct LinuxSysfsProvider {
    roots: LinuxRoots,
    include_software_adapters: bool,
    records: RwLock<BTreeMap<String, LinuxDeviceRecord>>,
    diagnostic: RwLock<DiagnosticState>,
}

impl LinuxSysfsProvider {
    pub fn new(roots: LinuxRoots, include_software_adapters: bool) -> Self {
        Self {
            roots,
            include_software_adapters,
            records: RwLock::new(BTreeMap::new()),
            diagnostic: RwLock::new(DiagnosticState::default()),
        }
    }

    fn record_for(&self, device: &CanonicalGpu) -> Option<LinuxDeviceRecord> {
        let provider_device_id = device.provider_device_ids.get(PROVIDER_ID)?;
        self.records.read().get(provider_device_id).cloned()
    }
}

impl InventoryProvider for LinuxSysfsProvider {
    fn provider_id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn enumerate(&self) -> Result<Vec<DeviceObservation>> {
        let inventory = discover(&self.roots, self.include_software_adapters);
        let observations = inventory
            .records
            .iter()
            .map(|record| record.observation.clone())
            .collect();
        let records = inventory
            .records
            .into_iter()
            .map(|record| (record.provider_device_id.clone(), record))
            .collect::<BTreeMap<_, _>>();
        let mut diagnostic = self.diagnostic.write();
        diagnostic.loaded = inventory.sysfs_available;
        diagnostic.devices_matched = records.len();
        diagnostic.reason =
            (!inventory.sysfs_available).then_some(UnavailableReason::TemporarilyUnavailable);
        diagnostic.message = if !inventory.sysfs_available {
            Some("PCI and DRM sysfs roots were unavailable".into())
        } else if inventory.warnings.is_empty() {
            None
        } else {
            Some(inventory.warnings.join("; "))
        };
        *self.records.write() = records;
        Ok(observations)
    }

    fn diagnostic(&self) -> ProviderDiagnostic {
        let diagnostic = self.diagnostic.read();
        ProviderDiagnostic {
            id: PROVIDER_ID.into(),
            loaded: diagnostic.loaded,
            version: Some("linux-kernel-sysfs".into()),
            devices_matched: diagnostic.devices_matched,
            reason: diagnostic.reason,
            message: diagnostic.message.clone(),
        }
    }
}

impl TelemetryProvider for LinuxSysfsProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata::new(PROVIDER_ID, 65, 90)
            .prefer(MetricKey::UtilizationOverall, 10)
            .prefer(MetricKey::MemoryBandwidthUtilizationPercent, 10)
            .prefer(MetricKey::MemoryDedicatedUsedBytes, 10)
            .prefer(MetricKey::MemorySharedUsedBytes, 10)
            .prefer(MetricKey::TemperatureCoreCelsius, 8)
            .prefer(MetricKey::TemperatureEdgeCelsius, 10)
            .prefer(MetricKey::TemperatureHotspotCelsius, 10)
            .prefer(MetricKey::TemperatureMemoryCelsius, 10)
            .prefer(MetricKey::PowerDrawWatts, 8)
            .prefer(MetricKey::PowerLimitWatts, 8)
            .prefer(MetricKey::PowerEnergyJoules, 8)
            .prefer(MetricKey::ClockGraphicsMhz, 8)
            .prefer(MetricKey::ClockMemoryMhz, 8)
            .prefer(MetricKey::FanPercent, 8)
            .prefer(MetricKey::FanRpm, 8)
    }

    fn capabilities(&self, device: &CanonicalGpu) -> CapabilitySet {
        self.record_for(device)
            .map_or_else(CapabilitySet::default, |record| {
                record.observation.capabilities
            })
    }

    fn sample(&self, device: &CanonicalGpu, request: &SampleRequest) -> Result<ProviderSample> {
        let Some(record) = self.record_for(device) else {
            return Ok(ProviderSample::default());
        };
        let sampled_at = now_millis();
        let mut sample = ProviderSample::default();

        for (key, sources) in &record.metric_sources {
            if !request.wants(*key) {
                continue;
            }
            if !record.device_path.exists() {
                sample.unavailable.push(UnavailableObservation {
                    device_id: device.identity.id.clone(),
                    metric: *key,
                    reason: UnavailableReason::DeviceLost,
                    source: Some(PROVIDER_ID.into()),
                    message: Some(format!(
                        "Linux GPU sysfs device {} disappeared",
                        record.device_path.display()
                    )),
                });
                continue;
            }

            let mut errors: Vec<MetricReadError> = Vec::new();
            let mut selected = None;
            for source in sources {
                match source.read(&record.device_path) {
                    Ok(value) => {
                        selected = Some((source, value));
                        break;
                    }
                    Err(error) => errors.push(error),
                }
            }

            if let Some((source, value)) = selected {
                sample.metrics.push(MetricObservation {
                    device_id: device.identity.id.clone(),
                    metric: *key,
                    value,
                    source: PROVIDER_ID.into(),
                    quality: source.quality,
                    sampled_at,
                    interval_ms: None,
                    definition: Some(source.definition.clone()),
                });
            } else if let Some(error) = select_error(errors) {
                sample.unavailable.push(UnavailableObservation {
                    device_id: device.identity.id.clone(),
                    metric: *key,
                    reason: error.reason,
                    source: Some(PROVIDER_ID.into()),
                    message: Some(error.message),
                });
            }
        }
        Ok(sample)
    }

    fn vendor_info(&self, device: &CanonicalGpu) -> Result<serde_json::Value> {
        Ok(self
            .record_for(device)
            .map_or(serde_json::Value::Null, |record| vendor_info(&record)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correlation::correlate;
    use crate::model::{MetricValue, UnavailableReason};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture {
        root: PathBuf,
        roots: LinuxRoots,
        device: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "let-smi-linux-provider-{}-{nonce}",
                std::process::id()
            ));
            let roots = LinuxRoots::new(root.join("sys"), root.join("proc"), root.join("dev"));
            let device = roots.sys.join("devices/pci0000:00/0000:03:00.0");
            fs::create_dir_all(roots.pci_devices()).expect("PCI fixture root");
            fs::create_dir_all(roots.drm_class()).expect("DRM fixture root");
            fs::create_dir_all(&device).expect("device fixture");
            write(&device.join("class"), "0x030000\n");
            write(&device.join("vendor"), "0x1002\n");
            write(&device.join("device"), "0x744c\n");
            write(&device.join("uevent"), "DRIVER=amdgpu\n");
            write(&device.join("gpu_busy_percent"), "0\n");
            #[cfg(unix)]
            std::os::unix::fs::symlink(&device, roots.pci_devices().join("0000:03:00.0"))
                .expect("PCI device symlink");
            Self {
                root,
                roots,
                device,
            }
        }

        fn gpu(&self, provider: &LinuxSysfsProvider) -> CanonicalGpu {
            correlate(provider.enumerate().expect("enumeration"))
                .into_iter()
                .next()
                .expect("GPU")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).expect("remove fixture");
        }
    }

    fn write(path: &Path, value: &str) {
        fs::write(path, value).expect("fixture value");
    }

    #[test]
    fn zero_busy_is_an_available_metric() {
        let fixture = Fixture::new();
        let provider = LinuxSysfsProvider::new(fixture.roots.clone(), false);
        let gpu = fixture.gpu(&provider);
        let sample = provider
            .sample(&gpu, &SampleRequest::default())
            .expect("sample");
        let utilization = sample
            .metrics
            .iter()
            .find(|metric| metric.metric == MetricKey::UtilizationOverall)
            .expect("utilization");
        assert_eq!(utilization.value, MetricValue::Number(0.0));
    }

    #[test]
    fn disappearing_device_is_reported_as_device_lost() {
        let fixture = Fixture::new();
        let provider = LinuxSysfsProvider::new(fixture.roots.clone(), false);
        let gpu = fixture.gpu(&provider);
        fs::remove_file(fixture.roots.pci_devices().join("0000:03:00.0"))
            .expect("remove PCI symlink");
        fs::remove_dir_all(&fixture.device).expect("remove device");
        let sample = provider
            .sample(&gpu, &SampleRequest::default())
            .expect("sample");
        assert_eq!(sample.unavailable.len(), 1);
        assert_eq!(sample.unavailable[0].reason, UnavailableReason::DeviceLost);
    }
}
