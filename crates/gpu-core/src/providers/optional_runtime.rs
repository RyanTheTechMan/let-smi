use crate::error::Result;
use crate::model::{
    CanonicalGpu, CapabilitySet, DeviceObservation, ProviderDiagnostic, ProviderSample,
    SampleRequest, UnavailableReason,
};
use crate::provider::{InventoryProvider, ProviderMetadata, TelemetryProvider};

/// Diagnostic-only boundary for a runtime whose stable adapter is unavailable.
///
/// This is deliberately not a fake telemetry implementation. It lets the monitor
/// explain a known, hardware-dependent gap without advertising capabilities.
pub struct UnavailableProvider {
    id: &'static str,
    reason: UnavailableReason,
    message: String,
}

impl UnavailableProvider {
    pub fn new(id: &'static str, reason: UnavailableReason, message: impl Into<String>) -> Self {
        Self {
            id,
            reason,
            message: message.into(),
        }
    }
}

impl InventoryProvider for UnavailableProvider {
    fn provider_id(&self) -> &'static str {
        self.id
    }

    fn enumerate(&self) -> Result<Vec<DeviceObservation>> {
        Ok(Vec::new())
    }

    fn diagnostic(&self) -> ProviderDiagnostic {
        ProviderDiagnostic {
            id: self.id.into(),
            loaded: false,
            version: None,
            devices_matched: 0,
            reason: Some(self.reason),
            message: Some(self.message.clone()),
        }
    }
}

impl TelemetryProvider for UnavailableProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata::new(self.id, 0, 0)
    }

    fn capabilities(&self, _device: &CanonicalGpu) -> CapabilitySet {
        CapabilitySet::default()
    }

    fn sample(&self, _device: &CanonicalGpu, _request: &SampleRequest) -> Result<ProviderSample> {
        Ok(ProviderSample::default())
    }
}
