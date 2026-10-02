use crate::merge::MergedMetrics;
use crate::model::{
    CanonicalGpu, GpuProcessSnapshot, MemoryTopology, Metric, MetricKey, MetricObservation,
    ProviderId, SampleRequest, TimestampMs, UnavailableReason,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuDeviceSnapshot {
    pub device_id: String,
    pub snapshot: GpuSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuMonitorSnapshot {
    pub sampled_at: TimestampMs,
    pub gpus: Vec<GpuDeviceSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuSnapshot {
    pub sampled_at: TimestampMs,
    pub utilization: UtilizationSnapshot,
    pub memory: MemorySnapshot,
    pub temperatures: TemperatureSnapshot,
    pub power: PowerSnapshot,
    pub clocks: ClockSnapshot,
    pub fan: FanSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processes: Option<Vec<GpuProcessSnapshot>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UtilizationSnapshot {
    pub overall: Metric<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graphics: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compute: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_controller: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoder: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decoder: Option<Metric<f64>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySnapshot {
    pub topology: MemoryTopology,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dedicated_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dedicated_used_bytes: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_used_bytes: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unified_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unified_used_bytes: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_bytes: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bandwidth_utilization_percent: Option<Metric<f64>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemperatureSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub core_celsius: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_celsius: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hotspot_celsius: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_celsius: Option<Metric<f64>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draw_watts: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_watts: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub energy_joules: Option<Metric<f64>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockSnapshot {
    #[serde(rename = "graphicsMHz", skip_serializing_if = "Option::is_none")]
    pub graphics_mhz: Option<Metric<f64>>,
    #[serde(rename = "computeMHz", skip_serializing_if = "Option::is_none")]
    pub compute_mhz: Option<Metric<f64>>,
    #[serde(rename = "memoryMHz", skip_serializing_if = "Option::is_none")]
    pub memory_mhz: Option<Metric<f64>>,
    #[serde(rename = "videoMHz", skip_serializing_if = "Option::is_none")]
    pub video_mhz: Option<Metric<f64>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<Metric<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm: Option<Metric<f64>>,
}

pub fn build_snapshot(
    gpu: &CanonicalGpu,
    merged: &MergedMetrics,
    sampled_at: TimestampMs,
    processes: Option<Vec<GpuProcessSnapshot>>,
) -> GpuSnapshot {
    let metric = |key| metric_for(gpu, merged, key);
    GpuSnapshot {
        sampled_at,
        utilization: UtilizationSnapshot {
            overall: metric_for_required(gpu, merged, MetricKey::UtilizationOverall),
            graphics: metric(MetricKey::UtilizationGraphics),
            compute: metric(MetricKey::UtilizationCompute),
            copy: metric(MetricKey::UtilizationCopy),
            memory_controller: metric(MetricKey::UtilizationMemoryController),
            encoder: metric(MetricKey::UtilizationEncoder),
            decoder: metric(MetricKey::UtilizationDecoder),
        },
        memory: MemorySnapshot {
            topology: gpu.memory.topology,
            dedicated_total_bytes: gpu.memory.dedicated_total_bytes,
            dedicated_used_bytes: metric(MetricKey::MemoryDedicatedUsedBytes),
            shared_total_bytes: gpu.memory.shared_total_bytes,
            shared_used_bytes: metric(MetricKey::MemorySharedUsedBytes),
            unified_total_bytes: gpu.memory.unified_total_bytes,
            unified_used_bytes: metric(MetricKey::MemoryUnifiedUsedBytes),
            budget_bytes: metric(MetricKey::MemoryBudgetBytes),
            bandwidth_utilization_percent: metric(MetricKey::MemoryBandwidthUtilizationPercent),
        },
        temperatures: TemperatureSnapshot {
            core_celsius: metric(MetricKey::TemperatureCoreCelsius),
            edge_celsius: metric(MetricKey::TemperatureEdgeCelsius),
            hotspot_celsius: metric(MetricKey::TemperatureHotspotCelsius),
            memory_celsius: metric(MetricKey::TemperatureMemoryCelsius),
        },
        power: PowerSnapshot {
            draw_watts: metric(MetricKey::PowerDrawWatts),
            limit_watts: metric(MetricKey::PowerLimitWatts),
            energy_joules: metric(MetricKey::PowerEnergyJoules),
        },
        clocks: ClockSnapshot {
            graphics_mhz: metric(MetricKey::ClockGraphicsMhz),
            compute_mhz: metric(MetricKey::ClockComputeMhz),
            memory_mhz: metric(MetricKey::ClockMemoryMhz),
            video_mhz: metric(MetricKey::ClockVideoMhz),
        },
        fan: FanSnapshot {
            percent: metric(MetricKey::FanPercent),
            rpm: metric(MetricKey::FanRpm),
        },
        processes,
    }
}

// Shared polls collect scalar fields once. Preserve the Rust caller's selection
// and the ordinary unavailable/omitted shape for fields it did not request.
pub(crate) fn filter_snapshot(
    snapshot: &mut GpuSnapshot,
    gpu: &CanonicalGpu,
    request: &SampleRequest,
) {
    if request.metrics.is_some() {
        let empty = build_snapshot(gpu, &MergedMetrics::default(), snapshot.sampled_at, None);
        macro_rules! filter {
            ($key:ident, $group:ident.$field:ident) => {
                if !request.wants(MetricKey::$key) {
                    snapshot.$group.$field = empty.$group.$field;
                }
            };
        }
        filter!(UtilizationOverall, utilization.overall);
        filter!(UtilizationGraphics, utilization.graphics);
        filter!(UtilizationCompute, utilization.compute);
        filter!(UtilizationCopy, utilization.copy);
        filter!(UtilizationMemoryController, utilization.memory_controller);
        filter!(UtilizationEncoder, utilization.encoder);
        filter!(UtilizationDecoder, utilization.decoder);
        filter!(MemoryDedicatedUsedBytes, memory.dedicated_used_bytes);
        filter!(MemorySharedUsedBytes, memory.shared_used_bytes);
        filter!(MemoryUnifiedUsedBytes, memory.unified_used_bytes);
        filter!(MemoryBudgetBytes, memory.budget_bytes);
        filter!(
            MemoryBandwidthUtilizationPercent,
            memory.bandwidth_utilization_percent
        );
        filter!(TemperatureCoreCelsius, temperatures.core_celsius);
        filter!(TemperatureEdgeCelsius, temperatures.edge_celsius);
        filter!(TemperatureHotspotCelsius, temperatures.hotspot_celsius);
        filter!(TemperatureMemoryCelsius, temperatures.memory_celsius);
        filter!(PowerDrawWatts, power.draw_watts);
        filter!(PowerLimitWatts, power.limit_watts);
        filter!(PowerEnergyJoules, power.energy_joules);
        filter!(ClockGraphicsMhz, clocks.graphics_mhz);
        filter!(ClockComputeMhz, clocks.compute_mhz);
        filter!(ClockMemoryMhz, clocks.memory_mhz);
        filter!(ClockVideoMhz, clocks.video_mhz);
        filter!(FanPercent, fan.percent);
        filter!(FanRpm, fan.rpm);
    }
    if !request.include_processes || !request.wants(MetricKey::Processes) {
        snapshot.processes = None;
    }
}

fn metric_for(gpu: &CanonicalGpu, merged: &MergedMetrics, key: MetricKey) -> Option<Metric<f64>> {
    if let Some(observation) = merged.selected.get(&key) {
        return Some(available(observation));
    }
    if let Some(observation) = merged.unavailable.get(&key) {
        return Some(Metric::unavailable(
            observation.reason,
            observation.source.clone(),
            observation.message.clone(),
        ));
    }
    gpu.capability_set.supports(key).then(|| {
        Metric::unavailable(
            UnavailableReason::TemporarilyUnavailable,
            None,
            Some("no provider returned a value for this sample".into()),
        )
    })
}

fn metric_for_required(gpu: &CanonicalGpu, merged: &MergedMetrics, key: MetricKey) -> Metric<f64> {
    metric_for(gpu, merged, key)
        .unwrap_or_else(|| Metric::unavailable(UnavailableReason::Unsupported, None, None))
}

fn available(observation: &MetricObservation) -> Metric<f64> {
    observation.value.as_f64().map_or_else(
        || {
            Metric::unavailable(
                UnavailableReason::ProviderError,
                Some(observation.source.clone()),
                Some("provider returned a non-numeric value".into()),
            )
        },
        |value| {
            Metric::available(
                value,
                observation.source.clone(),
                observation.quality,
                observation.sampled_at,
                observation.interval_ms,
                observation.definition.clone(),
            )
        },
    )
}

pub fn unavailable_from_provider(
    reason: UnavailableReason,
    source: ProviderId,
    message: impl Into<String>,
) -> Metric<f64> {
    Metric::unavailable(reason, Some(source), Some(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CapabilitySet, GpuIdentity, GpuKind, GpuVendor, MetricQuality, MetricValue,
        StaticMemoryInfo,
    };
    use std::collections::BTreeMap;

    fn gpu() -> CanonicalGpu {
        let capabilities = CapabilitySet::new([MetricKey::UtilizationOverall]);
        CanonicalGpu {
            identity: GpuIdentity {
                id: "gpu".into(),
                vendor: GpuVendor::Unknown,
                name: "GPU".into(),
                architecture: None,
                driver_version: None,
                firmware_version: None,
                kind: GpuKind::Unknown,
                uuid: None,
                pci: None,
                windows: None,
                macos: None,
                parent_device_id: None,
                partition: None,
            },
            capabilities: capabilities.to_public(),
            memory: StaticMemoryInfo::default(),
            providers: vec![],
            vendor_info: BTreeMap::new(),
            capability_set: capabilities,
            provider_device_ids: BTreeMap::new(),
        }
    }

    #[test]
    fn zero_is_available_and_not_a_sentinel() {
        let observation = MetricObservation {
            device_id: "gpu".into(),
            metric: MetricKey::UtilizationOverall,
            value: MetricValue::Number(0.0),
            source: "mock".into(),
            quality: MetricQuality::Direct,
            sampled_at: 1,
            interval_ms: None,
            definition: None,
        };
        let merged = MergedMetrics {
            selected: BTreeMap::from([(MetricKey::UtilizationOverall, observation)]),
            ..MergedMetrics::default()
        };
        let snapshot = build_snapshot(&gpu(), &merged, 1, None);
        assert!(snapshot.utilization.overall.is_available());
        match snapshot.utilization.overall {
            Metric::Available(value) => assert_eq!(value.value, 0.0),
            Metric::Unavailable(_) => panic!("zero must be available"),
        }
    }

    #[test]
    fn unsupported_is_explicitly_unavailable() {
        let mut gpu = gpu();
        gpu.capability_set = CapabilitySet::default();
        gpu.capabilities = gpu.capability_set.to_public();
        let snapshot = build_snapshot(&gpu, &MergedMetrics::default(), 1, None);
        match snapshot.utilization.overall {
            Metric::Unavailable(value) => {
                assert_eq!(value.reason, UnavailableReason::Unsupported);
                assert!(!value.available);
            }
            Metric::Available(_) => panic!("unsupported metric must not be zero"),
        }
    }

    #[test]
    fn clock_wire_names_preserve_the_public_mhz_acronym() {
        let observation = MetricObservation {
            device_id: "gpu".into(),
            metric: MetricKey::ClockGraphicsMhz,
            value: MetricValue::Number(1_500.0),
            source: "mock".into(),
            quality: MetricQuality::Direct,
            sampled_at: 1,
            interval_ms: None,
            definition: None,
        };
        let merged = MergedMetrics {
            selected: BTreeMap::from([(MetricKey::ClockGraphicsMhz, observation)]),
            ..MergedMetrics::default()
        };
        let value = serde_json::to_value(build_snapshot(&gpu(), &merged, 1, None)).unwrap();
        assert!(value["clocks"].get("graphicsMHz").is_some());
        assert!(value["clocks"].get("graphicsMhz").is_none());
    }
}
