use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub type DeviceId = String;
pub type ProviderId = String;
pub type TimestampMs = u64;

pub fn now_millis() -> TimestampMs {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Unknown,
}

impl GpuVendor {
    pub fn from_pci_vendor_id(vendor_id: u32) -> Self {
        match vendor_id {
            0x10de => Self::Nvidia,
            0x1002 | 0x1022 => Self::Amd,
            0x8086 => Self::Intel,
            0x106b => Self::Apple,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for GpuVendor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Nvidia => "nvidia",
            Self::Amd => "amd",
            Self::Intel => "intel",
            Self::Apple => "apple",
            Self::Unknown => "unknown",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GpuKind {
    Integrated,
    Discrete,
    External,
    Virtual,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PciIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    pub vendor_id: u32,
    pub device_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subsystem_vendor_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subsystem_device_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WindowsIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub luid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pnp_device_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MacosIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry_entry_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metal_registry_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PartitionType {
    Mig,
    Vgpu,
    Sriov,
    Tile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartitionIdentity {
    #[serde(rename = "type")]
    pub partition_type: PartitionType,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuIdentity {
    pub id: DeviceId,
    pub vendor: GpuVendor,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware_version: Option<String>,
    pub kind: GpuKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pci: Option<PciIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub windows: Option<WindowsIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macos: Option<MacosIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition: Option<PartitionIdentity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MemoryTopology {
    Dedicated,
    Shared,
    Unified,
    Mixed,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StaticMemoryInfo {
    pub topology: MemoryTopology,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dedicated_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unified_total_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub enum MetricKey {
    #[serde(rename = "utilization.overall")]
    UtilizationOverall,
    #[serde(rename = "utilization.graphics")]
    UtilizationGraphics,
    #[serde(rename = "utilization.compute")]
    UtilizationCompute,
    #[serde(rename = "utilization.copy")]
    UtilizationCopy,
    #[serde(rename = "utilization.memoryController")]
    UtilizationMemoryController,
    #[serde(rename = "utilization.encoder")]
    UtilizationEncoder,
    #[serde(rename = "utilization.decoder")]
    UtilizationDecoder,
    #[serde(rename = "memory.dedicatedUsedBytes")]
    MemoryDedicatedUsedBytes,
    #[serde(rename = "memory.sharedUsedBytes")]
    MemorySharedUsedBytes,
    #[serde(rename = "memory.unifiedUsedBytes")]
    MemoryUnifiedUsedBytes,
    #[serde(rename = "memory.budgetBytes")]
    MemoryBudgetBytes,
    #[serde(rename = "memory.bandwidthUtilizationPercent")]
    MemoryBandwidthUtilizationPercent,
    #[serde(rename = "temperatures.coreCelsius")]
    TemperatureCoreCelsius,
    #[serde(rename = "temperatures.edgeCelsius")]
    TemperatureEdgeCelsius,
    #[serde(rename = "temperatures.hotspotCelsius")]
    TemperatureHotspotCelsius,
    #[serde(rename = "temperatures.memoryCelsius")]
    TemperatureMemoryCelsius,
    #[serde(rename = "power.drawWatts")]
    PowerDrawWatts,
    #[serde(rename = "power.limitWatts")]
    PowerLimitWatts,
    #[serde(rename = "power.energyJoules")]
    PowerEnergyJoules,
    #[serde(rename = "clocks.graphicsMHz")]
    ClockGraphicsMhz,
    #[serde(rename = "clocks.computeMHz")]
    ClockComputeMhz,
    #[serde(rename = "clocks.memoryMHz")]
    ClockMemoryMhz,
    #[serde(rename = "clocks.videoMHz")]
    ClockVideoMhz,
    #[serde(rename = "fan.percent")]
    FanPercent,
    #[serde(rename = "fan.rpm")]
    FanRpm,
    #[serde(rename = "processes")]
    Processes,
}

impl MetricKey {
    pub const ALL: [Self; 25] = [
        Self::UtilizationOverall,
        Self::UtilizationGraphics,
        Self::UtilizationCompute,
        Self::UtilizationCopy,
        Self::UtilizationMemoryController,
        Self::UtilizationEncoder,
        Self::UtilizationDecoder,
        Self::MemoryDedicatedUsedBytes,
        Self::MemorySharedUsedBytes,
        Self::MemoryUnifiedUsedBytes,
        Self::MemoryBudgetBytes,
        Self::MemoryBandwidthUtilizationPercent,
        Self::TemperatureCoreCelsius,
        Self::TemperatureEdgeCelsius,
        Self::TemperatureHotspotCelsius,
        Self::TemperatureMemoryCelsius,
        Self::PowerDrawWatts,
        Self::PowerLimitWatts,
        Self::PowerEnergyJoules,
        Self::ClockGraphicsMhz,
        Self::ClockComputeMhz,
        Self::ClockMemoryMhz,
        Self::ClockVideoMhz,
        Self::FanPercent,
        Self::FanRpm,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricQuality {
    Direct,
    Derived,
    Estimated,
}

impl MetricQuality {
    pub fn score(self) -> i64 {
        match self {
            Self::Direct => 300,
            Self::Derived => 200,
            Self::Estimated => 100,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnavailableReason {
    Unsupported,
    DriverLibraryMissing,
    PermissionDenied,
    DeviceLost,
    FirstSample,
    TemporarilyUnavailable,
    ProviderError,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MetricValue {
    Number(f64),
    Integer(u64),
    Boolean(bool),
    Text(String),
}

impl MetricValue {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            Self::Integer(value) => Some(*value as f64),
            Self::Boolean(_) | Self::Text(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricObservation {
    pub device_id: DeviceId,
    pub metric: MetricKey,
    pub value: MetricValue,
    pub source: ProviderId,
    pub quality: MetricQuality,
    pub sampled_at: TimestampMs,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnavailableObservation {
    pub device_id: DeviceId,
    pub metric: MetricKey,
    pub reason: UnavailableReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<ProviderId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableMetric<T> {
    pub available: bool,
    pub value: T,
    pub source: ProviderId,
    pub quality: MetricQuality,
    pub sampled_at: TimestampMs,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnavailableMetric {
    pub available: bool,
    pub reason: UnavailableReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<ProviderId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Metric<T> {
    Available(AvailableMetric<T>),
    Unavailable(UnavailableMetric),
}

impl<T> Metric<T> {
    pub fn available(
        value: T,
        source: ProviderId,
        quality: MetricQuality,
        sampled_at: TimestampMs,
        interval_ms: Option<u64>,
        definition: Option<String>,
    ) -> Self {
        Self::Available(AvailableMetric {
            available: true,
            value,
            source,
            quality,
            sampled_at,
            interval_ms,
            definition,
        })
    }

    pub fn unavailable(
        reason: UnavailableReason,
        source: Option<ProviderId>,
        message: Option<String>,
    ) -> Self {
        Self::Unavailable(UnavailableMetric {
            available: false,
            reason,
            source,
            message,
        })
    }

    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilitySet {
    metrics: BTreeSet<MetricKey>,
    pub vendor_extensions: BTreeSet<String>,
}

impl CapabilitySet {
    pub fn new(metrics: impl IntoIterator<Item = MetricKey>) -> Self {
        Self {
            metrics: metrics.into_iter().collect(),
            vendor_extensions: BTreeSet::new(),
        }
    }

    pub fn with_extension(mut self, extension: impl Into<String>) -> Self {
        self.vendor_extensions.insert(extension.into());
        self
    }

    pub fn insert(&mut self, metric: MetricKey) {
        self.metrics.insert(metric);
    }

    pub fn supports(&self, metric: MetricKey) -> bool {
        self.metrics.contains(&metric)
    }

    pub fn metrics(&self) -> impl Iterator<Item = MetricKey> + '_ {
        self.metrics.iter().copied()
    }

    pub fn extend(&mut self, other: &Self) {
        self.metrics.extend(other.metrics.iter().copied());
        self.vendor_extensions
            .extend(other.vendor_extensions.iter().cloned());
    }

    pub fn to_public(&self) -> GpuCapabilities {
        let any = |keys: &[MetricKey]| keys.iter().any(|key| self.supports(*key));
        GpuCapabilities {
            metrics: self.metrics().collect(),
            utilization: UtilizationCapabilities {
                overall: self.supports(MetricKey::UtilizationOverall),
                graphics: self.supports(MetricKey::UtilizationGraphics),
                compute: self.supports(MetricKey::UtilizationCompute),
                copy: self.supports(MetricKey::UtilizationCopy),
                memory_controller: self.supports(MetricKey::UtilizationMemoryController),
                encoder: self.supports(MetricKey::UtilizationEncoder),
                decoder: self.supports(MetricKey::UtilizationDecoder),
            },
            temperature: any(&[
                MetricKey::TemperatureCoreCelsius,
                MetricKey::TemperatureEdgeCelsius,
                MetricKey::TemperatureHotspotCelsius,
                MetricKey::TemperatureMemoryCelsius,
            ]),
            power: any(&[
                MetricKey::PowerDrawWatts,
                MetricKey::PowerLimitWatts,
                MetricKey::PowerEnergyJoules,
            ]),
            clocks: any(&[
                MetricKey::ClockGraphicsMhz,
                MetricKey::ClockComputeMhz,
                MetricKey::ClockMemoryMhz,
                MetricKey::ClockVideoMhz,
            ]),
            fan: any(&[MetricKey::FanPercent, MetricKey::FanRpm]),
            processes: self.supports(MetricKey::Processes),
            vendor_extensions: self.vendor_extensions.iter().cloned().collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UtilizationCapabilities {
    pub overall: bool,
    pub graphics: bool,
    pub compute: bool,
    pub copy: bool,
    pub memory_controller: bool,
    pub encoder: bool,
    pub decoder: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuCapabilities {
    pub metrics: Vec<MetricKey>,
    pub utilization: UtilizationCapabilities,
    pub temperature: bool,
    pub power: bool,
    pub clocks: bool,
    pub fan: bool,
    pub processes: bool,
    pub vendor_extensions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceObservation {
    pub provider: ProviderId,
    pub provider_device_id: String,
    pub observed_at: TimestampMs,
    pub vendor: GpuVendor,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware_version: Option<String>,
    pub kind: GpuKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pci: Option<PciIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub windows: Option<WindowsIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macos: Option<MacosIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_provider_device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition: Option<PartitionIdentity>,
    pub memory: StaticMemoryInfo,
    #[serde(skip)]
    pub capabilities: CapabilitySet,
    pub identity_priority: i16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enumeration_ordinal: Option<u32>,
    #[serde(default)]
    pub vendor_info: BTreeMap<String, serde_json::Value>,
}

impl DeviceObservation {
    pub fn new(
        provider: impl Into<String>,
        provider_device_id: impl Into<String>,
        vendor: GpuVendor,
        name: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            provider_device_id: provider_device_id.into(),
            observed_at: now_millis(),
            vendor,
            name: name.into(),
            architecture: None,
            driver_version: None,
            firmware_version: None,
            kind: GpuKind::Unknown,
            uuid: None,
            pci: None,
            windows: None,
            macos: None,
            parent_provider_device_id: None,
            partition: None,
            memory: StaticMemoryInfo::default(),
            capabilities: CapabilitySet::default(),
            identity_priority: 0,
            enumeration_ordinal: None,
            vendor_info: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalGpu {
    pub identity: GpuIdentity,
    pub capabilities: GpuCapabilities,
    pub memory: StaticMemoryInfo,
    pub providers: Vec<String>,
    pub vendor_info: BTreeMap<String, serde_json::Value>,
    #[serde(skip)]
    pub capability_set: CapabilitySet,
    #[serde(skip)]
    pub provider_device_ids: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SampleRequest {
    #[serde(default = "default_sample_window_ms")]
    pub window_ms: u64,
    #[serde(default)]
    pub metrics: Option<BTreeSet<MetricKey>>,
    #[serde(default)]
    pub include_processes: bool,
}

const fn default_sample_window_ms() -> u64 {
    1_000
}

impl Default for SampleRequest {
    fn default() -> Self {
        Self {
            window_ms: 1_000,
            metrics: None,
            include_processes: false,
        }
    }
}

impl SampleRequest {
    pub fn wants(&self, metric: MetricKey) -> bool {
        self.metrics
            .as_ref()
            .is_none_or(|metrics| metrics.contains(&metric))
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProviderSample {
    pub metrics: Vec<MetricObservation>,
    pub unavailable: Vec<UnavailableObservation>,
    /// `Some([])` means a supported process query succeeded and found no
    /// processes. `None` means the provider did not supply process data.
    pub processes: Option<Vec<GpuProcessSnapshot>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuProcessSnapshot {
    pub pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_used_bytes: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utilization: Option<ProcessUtilization>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProcessUtilization {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overall: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graphics: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compute: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoder: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decoder: Option<Metric<f64>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDiagnostic {
    pub id: ProviderId,
    pub loaded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub devices_matched: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<UnavailableReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    pub platform: String,
    pub arch: String,
    pub providers: Vec<ProviderDiagnostic>,
    pub warnings: Vec<String>,
    pub metric_selections: Vec<DeviceMergeDiagnostics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeCandidateDiagnostic {
    pub source: ProviderId,
    pub score: i64,
    pub selected: bool,
    pub quality: MetricQuality,
    pub sampled_at: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricMergeDiagnostic {
    pub metric: MetricKey,
    pub candidates: Vec<MergeCandidateDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceMergeDiagnostics {
    pub device_id: DeviceId,
    pub metrics: Vec<MetricMergeDiagnostic>,
}
