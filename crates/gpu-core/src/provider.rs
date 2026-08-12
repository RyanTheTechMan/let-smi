use crate::error::Result;
use crate::model::{
    CanonicalGpu, CapabilitySet, DeviceObservation, MetricKey, ProviderDiagnostic, ProviderSample,
    SampleRequest,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct ProviderMetadata {
    pub id: &'static str,
    pub specificity: i16,
    pub reliability: i16,
    pub metric_priorities: BTreeMap<MetricKey, i16>,
}

impl ProviderMetadata {
    pub fn new(id: &'static str, specificity: i16, reliability: i16) -> Self {
        Self {
            id,
            specificity,
            reliability,
            metric_priorities: BTreeMap::new(),
        }
    }

    pub fn prefer(mut self, metric: MetricKey, priority: i16) -> Self {
        self.metric_priorities.insert(metric, priority);
        self
    }

    pub fn metric_priority(&self, metric: MetricKey) -> i16 {
        self.metric_priorities.get(&metric).copied().unwrap_or(0)
    }
}

pub trait InventoryProvider: Send + Sync {
    fn provider_id(&self) -> &'static str;

    fn enumerate(&self) -> Result<Vec<DeviceObservation>>;

    fn diagnostic(&self) -> ProviderDiagnostic;
}

pub trait TelemetryProvider: Send + Sync {
    fn metadata(&self) -> ProviderMetadata;

    fn capabilities(&self, device: &CanonicalGpu) -> CapabilitySet;

    fn sample(&self, device: &CanonicalGpu, request: &SampleRequest) -> Result<ProviderSample>;

    fn vendor_info(&self, device: &CanonicalGpu) -> Result<serde_json::Value> {
        let _ = device;
        Ok(serde_json::Value::Null)
    }

    fn shutdown(&self) {}
}

pub trait Provider: InventoryProvider + TelemetryProvider {}

impl<T> Provider for T where T: InventoryProvider + TelemetryProvider {}
