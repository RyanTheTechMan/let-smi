use crate::model::{
    MergeCandidateDiagnostic, MetricKey, MetricMergeDiagnostic, MetricObservation, ProviderId,
    UnavailableObservation, UnavailableReason,
};
use crate::provider::ProviderMetadata;
use std::collections::BTreeMap;

const STALE_AFTER_MS: u64 = 30_000;

#[derive(Debug, Default)]
pub struct MergedMetrics {
    pub selected: BTreeMap<MetricKey, MetricObservation>,
    pub unavailable: BTreeMap<MetricKey, UnavailableObservation>,
    pub diagnostics: Vec<MetricMergeDiagnostic>,
}

pub fn merge_metrics(
    metrics: Vec<MetricObservation>,
    unavailable: Vec<UnavailableObservation>,
    providers: &BTreeMap<ProviderId, ProviderMetadata>,
    now_ms: u64,
) -> MergedMetrics {
    let mut grouped: BTreeMap<MetricKey, Vec<MetricObservation>> = BTreeMap::new();
    let mut invalid: Vec<UnavailableObservation> = Vec::new();
    for metric in metrics {
        if validate_metric(&metric) {
            grouped.entry(metric.metric).or_default().push(metric);
        } else {
            invalid.push(UnavailableObservation {
                device_id: metric.device_id,
                metric: metric.metric,
                reason: UnavailableReason::ProviderError,
                source: Some(metric.source),
                message: Some("provider returned a non-finite or out-of-range value".into()),
            });
        }
    }

    let mut result = MergedMetrics::default();
    for (key, mut candidates) in grouped {
        candidates.sort_by(|left, right| {
            score(right, providers, now_ms)
                .cmp(&score(left, providers, now_ms))
                .then_with(|| left.source.cmp(&right.source))
        });

        let selected_index = candidates
            .iter()
            .position(|candidate| !is_stale(candidate, now_ms));

        let diagnostics = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| MergeCandidateDiagnostic {
                source: candidate.source.clone(),
                score: score(candidate, providers, now_ms),
                selected: Some(index) == selected_index,
                quality: candidate.quality,
                sampled_at: candidate.sampled_at,
            })
            .collect();
        result.diagnostics.push(MetricMergeDiagnostic {
            metric: key,
            candidates: diagnostics,
        });
        if let Some(selected) = selected_index.and_then(|index| candidates.get(index).cloned()) {
            result.selected.insert(key, selected);
        } else if let Some(stale) = candidates.first() {
            result.unavailable.insert(
                key,
                UnavailableObservation {
                    device_id: stale.device_id.clone(),
                    metric: key,
                    reason: UnavailableReason::TemporarilyUnavailable,
                    source: Some(stale.source.clone()),
                    message: Some("all provider observations for this metric are stale".into()),
                },
            );
        }
    }

    for candidate in unavailable.into_iter().chain(invalid) {
        if result.selected.contains_key(&candidate.metric) {
            continue;
        }
        let replace = result
            .unavailable
            .get(&candidate.metric)
            .is_none_or(|current| {
                unavailable_score(candidate.reason) > unavailable_score(current.reason)
            });
        if replace {
            result.unavailable.insert(candidate.metric, candidate);
        }
    }

    result
}

fn is_stale(observation: &MetricObservation, now_ms: u64) -> bool {
    now_ms.saturating_sub(observation.sampled_at) > STALE_AFTER_MS
}

fn score(
    observation: &MetricObservation,
    providers: &BTreeMap<ProviderId, ProviderMetadata>,
    now_ms: u64,
) -> i64 {
    let metadata = providers.get(&observation.source);
    let priority = metadata.map_or(0, |value| {
        i64::from(value.metric_priority(observation.metric))
    });
    let specificity = metadata.map_or(0, |value| i64::from(value.specificity));
    let reliability = metadata.map_or(0, |value| i64::from(value.reliability));
    let age = now_ms.saturating_sub(observation.sampled_at);
    let stale_penalty = if age > STALE_AFTER_MS {
        100_000 + i64::try_from(age.min(i64::MAX as u64)).unwrap_or(i64::MAX)
    } else {
        i64::try_from(age / 10).unwrap_or(i64::MAX)
    };

    priority * 100_000 + specificity * 1_000 + reliability * 10 + observation.quality.score()
        - stale_penalty
}

fn unavailable_score(reason: UnavailableReason) -> i8 {
    match reason {
        UnavailableReason::PermissionDenied => 7,
        UnavailableReason::DeviceLost => 6,
        UnavailableReason::DriverLibraryMissing => 5,
        UnavailableReason::FirstSample => 4,
        UnavailableReason::ProviderError => 3,
        UnavailableReason::TemporarilyUnavailable => 2,
        UnavailableReason::Unsupported => 1,
    }
}

fn validate_metric(observation: &MetricObservation) -> bool {
    let Some(value) = observation.value.as_f64() else {
        return false;
    };
    if !value.is_finite() {
        return false;
    }

    match observation.metric {
        MetricKey::UtilizationOverall
        | MetricKey::UtilizationGraphics
        | MetricKey::UtilizationCompute
        | MetricKey::UtilizationCopy
        | MetricKey::UtilizationMemoryController
        | MetricKey::UtilizationEncoder
        | MetricKey::UtilizationDecoder
        | MetricKey::MemoryBandwidthUtilizationPercent
        | MetricKey::FanPercent => (0.0..=100.0).contains(&value),
        MetricKey::MemoryDedicatedUsedBytes
        | MetricKey::MemorySharedUsedBytes
        | MetricKey::MemoryUnifiedUsedBytes
        | MetricKey::MemoryBudgetBytes
        | MetricKey::PowerDrawWatts
        | MetricKey::PowerLimitWatts
        | MetricKey::PowerEnergyJoules
        | MetricKey::ClockGraphicsMhz
        | MetricKey::ClockComputeMhz
        | MetricKey::ClockMemoryMhz
        | MetricKey::ClockVideoMhz
        | MetricKey::FanRpm => value >= 0.0,
        MetricKey::TemperatureCoreCelsius
        | MetricKey::TemperatureEdgeCelsius
        | MetricKey::TemperatureHotspotCelsius
        | MetricKey::TemperatureMemoryCelsius => (-100.0..=300.0).contains(&value),
        MetricKey::Processes => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MetricQuality, MetricValue};

    fn metric(source: &str, value: f64, quality: MetricQuality) -> MetricObservation {
        MetricObservation {
            device_id: "gpu".into(),
            metric: MetricKey::UtilizationOverall,
            value: MetricValue::Number(value),
            source: source.into(),
            quality,
            sampled_at: 1_000,
            interval_ms: Some(1_000),
            definition: None,
        }
    }

    #[test]
    fn provider_priority_is_per_metric() {
        let generic = ProviderMetadata::new("generic", 50, 90);
        let vendor =
            ProviderMetadata::new("vendor", 90, 90).prefer(MetricKey::UtilizationOverall, 10);
        let providers = BTreeMap::from([
            ("generic".to_owned(), generic),
            ("vendor".to_owned(), vendor),
        ]);
        let merged = merge_metrics(
            vec![
                metric("generic", 20.0, MetricQuality::Direct),
                metric("vendor", 75.0, MetricQuality::Derived),
            ],
            Vec::new(),
            &providers,
            1_000,
        );
        assert_eq!(
            merged
                .selected
                .get(&MetricKey::UtilizationOverall)
                .and_then(|value| value.value.as_f64()),
            Some(75.0)
        );
    }

    #[test]
    fn invalid_utilization_is_not_exposed() {
        let merged = merge_metrics(
            vec![metric("broken", 101.0, MetricQuality::Direct)],
            Vec::new(),
            &BTreeMap::new(),
            1_000,
        );
        assert!(!merged.selected.contains_key(&MetricKey::UtilizationOverall));
        assert_eq!(
            merged.unavailable[&MetricKey::UtilizationOverall].reason,
            UnavailableReason::ProviderError
        );
    }

    #[test]
    fn stale_metric_is_unavailable_instead_of_replayed() {
        let merged = merge_metrics(
            vec![metric("old-provider", 55.0, MetricQuality::Direct)],
            Vec::new(),
            &BTreeMap::new(),
            31_001,
        );

        assert!(!merged.selected.contains_key(&MetricKey::UtilizationOverall));
        assert_eq!(
            merged.unavailable[&MetricKey::UtilizationOverall].reason,
            UnavailableReason::TemporarilyUnavailable
        );
        assert!(
            merged.diagnostics[0]
                .candidates
                .iter()
                .all(|candidate| !candidate.selected)
        );
    }

    #[test]
    fn a_negative_celsius_reading_is_not_an_unavailable_sentinel() {
        let mut observation = metric("cold-provider", -5.0, MetricQuality::Direct);
        observation.metric = MetricKey::TemperatureCoreCelsius;
        let merged = merge_metrics(vec![observation], Vec::new(), &BTreeMap::new(), 1_000);
        assert_eq!(
            merged
                .selected
                .get(&MetricKey::TemperatureCoreCelsius)
                .and_then(|value| value.value.as_f64()),
            Some(-5.0)
        );
    }
}
