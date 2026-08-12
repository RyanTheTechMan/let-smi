use crate::error::Result;
use crate::model::{
    CanonicalGpu, CapabilitySet, DeviceObservation, ProviderDiagnostic, ProviderSample,
    SampleRequest,
};
use crate::provider::{InventoryProvider, ProviderMetadata, TelemetryProvider};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub struct MockProvider {
    metadata: ProviderMetadata,
    devices: Vec<DeviceObservation>,
    samples: Mutex<VecDeque<ProviderSample>>,
    fallback_sample: ProviderSample,
    sample_count: AtomicUsize,
    shutdown: AtomicBool,
}

impl MockProvider {
    pub fn new(
        metadata: ProviderMetadata,
        devices: Vec<DeviceObservation>,
        samples: Vec<ProviderSample>,
    ) -> Self {
        let fallback_sample = samples.last().cloned().unwrap_or_default();
        Self {
            metadata,
            devices,
            samples: Mutex::new(samples.into()),
            fallback_sample,
            sample_count: AtomicUsize::new(0),
            shutdown: AtomicBool::new(false),
        }
    }

    pub fn was_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    pub fn sample_count(&self) -> usize {
        self.sample_count.load(Ordering::Acquire)
    }
}

impl InventoryProvider for MockProvider {
    fn provider_id(&self) -> &'static str {
        self.metadata.id
    }

    fn enumerate(&self) -> Result<Vec<DeviceObservation>> {
        Ok(self.devices.clone())
    }

    fn diagnostic(&self) -> ProviderDiagnostic {
        ProviderDiagnostic {
            id: self.metadata.id.into(),
            loaded: true,
            version: Some("mock".into()),
            devices_matched: self.devices.len(),
            reason: None,
            message: None,
        }
    }
}

impl TelemetryProvider for MockProvider {
    fn metadata(&self) -> ProviderMetadata {
        self.metadata.clone()
    }

    fn capabilities(&self, device: &CanonicalGpu) -> CapabilitySet {
        if device.provider_device_ids.contains_key(self.metadata.id) {
            self.devices
                .iter()
                .find(|candidate| {
                    device
                        .provider_device_ids
                        .get(self.metadata.id)
                        .is_some_and(|id| id == &candidate.provider_device_id)
                })
                .map_or_else(CapabilitySet::default, |value| value.capabilities.clone())
        } else {
            CapabilitySet::default()
        }
    }

    fn sample(&self, _device: &CanonicalGpu, _request: &SampleRequest) -> Result<ProviderSample> {
        self.sample_count.fetch_add(1, Ordering::AcqRel);
        Ok(self
            .samples
            .lock()
            .pop_front()
            .unwrap_or_else(|| self.fallback_sample.clone()))
    }

    fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }
}
