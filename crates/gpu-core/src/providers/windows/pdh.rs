use super::dxgi::canonical_luid;
use crate::error::{GpuError, Result};
use crate::model::{
    CanonicalGpu, CapabilitySet, DeviceObservation, MetricKey, MetricObservation, MetricQuality,
    MetricValue, ProviderDiagnostic, ProviderSample, SampleRequest, UnavailableObservation,
    UnavailableReason, now_millis,
};
use crate::provider::{InventoryProvider, ProviderMetadata, TelemetryProvider};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::mem::{size_of, zeroed};
use std::time::Instant;
use windows::Win32::System::Performance::{
    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
    PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery,
    PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows::core::{PCWSTR, w};

const PROVIDER_ID: &str = "windows-pdh";
const GPU_ENGINE_COUNTER: PCWSTR = w!("\\GPU Engine(*)\\Utilization Percentage");

struct QueryState {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    primed: bool,
    last_collected: Option<Instant>,
}

// PDH query handles are opaque process handles. All calls and destruction are
// serialized through PdhProvider::state and performed by the monitor sampler.
unsafe impl Send for QueryState {}

impl Drop for QueryState {
    fn drop(&mut self) {
        if !self.query.is_invalid() {
            // SAFETY: query is owned by this state and closed once.
            unsafe {
                PdhCloseQuery(self.query);
            }
        }
    }
}

enum State {
    Ready(QueryState),
    Unavailable(String),
    Closed,
}

pub struct PdhProvider {
    state: Mutex<State>,
}

impl PdhProvider {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(open_query()),
        }
    }
}

impl InventoryProvider for PdhProvider {
    fn provider_id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn enumerate(&self) -> Result<Vec<DeviceObservation>> {
        Ok(Vec::new())
    }

    fn diagnostic(&self) -> ProviderDiagnostic {
        match &*self.state.lock() {
            State::Ready(_) => ProviderDiagnostic {
                id: PROVIDER_ID.into(),
                loaded: true,
                version: None,
                devices_matched: 0,
                reason: None,
                message: None,
            },
            State::Unavailable(message) => ProviderDiagnostic {
                id: PROVIDER_ID.into(),
                loaded: false,
                version: None,
                devices_matched: 0,
                reason: Some(UnavailableReason::Unsupported),
                message: Some(message.clone()),
            },
            State::Closed => ProviderDiagnostic {
                id: PROVIDER_ID.into(),
                loaded: false,
                version: None,
                devices_matched: 0,
                reason: Some(UnavailableReason::DeviceLost),
                message: Some("provider has been closed".into()),
            },
        }
    }
}

impl TelemetryProvider for PdhProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata::new(PROVIDER_ID, 60, 90).prefer(MetricKey::UtilizationOverall, 10)
    }

    fn capabilities(&self, device: &CanonicalGpu) -> CapabilitySet {
        if device
            .identity
            .windows
            .as_ref()
            .and_then(|windows| windows.luid.as_ref())
            .is_none()
            || !matches!(&*self.state.lock(), State::Ready(_))
        {
            return CapabilitySet::default();
        }
        CapabilitySet::new([
            MetricKey::UtilizationOverall,
            MetricKey::UtilizationGraphics,
            MetricKey::UtilizationCompute,
            MetricKey::UtilizationCopy,
            MetricKey::UtilizationEncoder,
            MetricKey::UtilizationDecoder,
        ])
    }

    fn sample(&self, device: &CanonicalGpu, _request: &SampleRequest) -> Result<ProviderSample> {
        let Some(luid) = device
            .identity
            .windows
            .as_ref()
            .and_then(|windows| windows.luid.as_deref())
        else {
            return Ok(ProviderSample::default());
        };
        let mut state = self.state.lock();
        let State::Ready(query) = &mut *state else {
            return Ok(ProviderSample::default());
        };

        // SAFETY: the query handle is live and serialized by the mutex.
        let status = unsafe { PdhCollectQueryData(query.query) };
        if status != 0 {
            return Err(GpuError::provider(
                PROVIDER_ID,
                UnavailableReason::TemporarilyUnavailable,
                format!("PdhCollectQueryData failed with 0x{status:08x}"),
            ));
        }
        let collected_at = Instant::now();
        let interval_ms = query.last_collected.replace(collected_at).map(|previous| {
            u64::try_from(collected_at.saturating_duration_since(previous).as_millis())
                .unwrap_or(u64::MAX)
        });
        if !query.primed {
            query.primed = true;
            return Ok(first_sample(device));
        }

        let items = formatted_items(query.counter)?;
        let engines = aggregate_engines(items, luid);
        if engines.is_empty() {
            return Ok(ProviderSample {
                unavailable: vec![UnavailableObservation {
                    device_id: device.identity.id.clone(),
                    metric: MetricKey::UtilizationOverall,
                    reason: UnavailableReason::TemporarilyUnavailable,
                    source: Some(PROVIDER_ID.into()),
                    message: Some("PDH returned no GPU engine counters for this adapter".into()),
                }],
                ..ProviderSample::default()
            });
        }
        Ok(engine_metrics(&device.identity.id, engines, interval_ms))
    }

    fn shutdown(&self) {
        *self.state.lock() = State::Closed;
    }
}

fn open_query() -> State {
    // SAFETY: zero is the documented invalid handle representation.
    let mut query: PDH_HQUERY = unsafe { zeroed() };
    // SAFETY: null data source selects real-time counters and query is an out pointer.
    let status = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &raw mut query) };
    if status != 0 {
        return State::Unavailable(format!("PdhOpenQueryW failed with 0x{status:08x}"));
    }
    // SAFETY: zero is the documented invalid handle representation.
    let mut counter: PDH_HCOUNTER = unsafe { zeroed() };
    // SAFETY: query is live, path is static nul-terminated UTF-16, and counter
    // is an out pointer.
    let status = unsafe { PdhAddEnglishCounterW(query, GPU_ENGINE_COUNTER, 0, &raw mut counter) };
    if status != 0 {
        // SAFETY: query was opened successfully and is not used again.
        unsafe {
            PdhCloseQuery(query);
        }
        return State::Unavailable(format!(
            "GPU Engine counters are unavailable (0x{status:08x})"
        ));
    }
    State::Ready(QueryState {
        query,
        counter,
        primed: false,
        last_collected: None,
    })
}

fn formatted_items(counter: PDH_HCOUNTER) -> Result<Vec<(String, f64)>> {
    let mut byte_count = 0_u32;
    let mut item_count = 0_u32;
    // SAFETY: null buffer is the documented size-query call.
    let status = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &raw mut byte_count,
            &raw mut item_count,
            None,
        )
    };
    if status != PDH_MORE_DATA && status != 0 {
        return Err(GpuError::provider(
            PROVIDER_ID,
            UnavailableReason::TemporarilyUnavailable,
            format!("PDH counter size query failed with 0x{status:08x}"),
        ));
    }
    if byte_count == 0 || item_count == 0 {
        return Ok(Vec::new());
    }
    let byte_count = usize::try_from(byte_count).unwrap_or(usize::MAX);
    let words = byte_count.div_ceil(size_of::<usize>());
    let mut buffer = vec![0_usize; words];
    let mut actual_bytes =
        u32::try_from(words.saturating_mul(size_of::<usize>())).unwrap_or(u32::MAX);
    // SAFETY: the usize allocation is sufficiently sized and aligned for the
    // PDH item array, and counts are writable out parameters.
    let status = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &raw mut actual_bytes,
            &raw mut item_count,
            Some(buffer.as_mut_ptr().cast()),
        )
    };
    if status != 0 {
        return Err(GpuError::provider(
            PROVIDER_ID,
            UnavailableReason::TemporarilyUnavailable,
            format!("PDH counter read failed with 0x{status:08x}"),
        ));
    }
    let count = usize::try_from(item_count).unwrap_or(0);
    // SAFETY: PDH wrote item_count contiguous entries into the aligned buffer.
    let items = unsafe {
        std::slice::from_raw_parts(buffer.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(), count)
    };
    let mut result = Vec::with_capacity(count);
    for item in items {
        if item.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA
            && item.FmtValue.CStatus != PDH_CSTATUS_NEW_DATA
        {
            continue;
        }
        // SAFETY: PDH_FMT_DOUBLE requests the doubleValue union member.
        let value = unsafe { item.FmtValue.Anonymous.doubleValue };
        if !value.is_finite() || !(0.0..=100.0).contains(&value) {
            continue;
        }
        // SAFETY: PDH owns a nul-terminated name valid until the buffer drops.
        let name = unsafe { item.szName.to_string() }.unwrap_or_default();
        result.push((name, value));
    }
    Ok(result)
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct EngineKey {
    physical: String,
    engine: String,
    engine_type: String,
}

fn aggregate_engines(items: Vec<(String, f64)>, wanted_luid: &str) -> HashMap<EngineKey, f64> {
    let mut result: HashMap<EngineKey, f64> = HashMap::new();
    for (name, value) in items {
        let Some(instance) = parse_instance(&name) else {
            continue;
        };
        if instance.luid != wanted_luid {
            continue;
        }
        let key = EngineKey {
            physical: instance.physical,
            engine: instance.engine,
            engine_type: instance.engine_type,
        };
        *result.entry(key).or_default() += value;
    }
    for value in result.values_mut() {
        *value = value.min(100.0);
    }
    result
}

struct Instance {
    luid: String,
    physical: String,
    engine: String,
    engine_type: String,
}

fn parse_instance(name: &str) -> Option<Instance> {
    let lowercase = name.to_ascii_lowercase();
    let luid_start = lowercase.find("luid_0x")? + "luid_0x".len();
    let (high, rest) = lowercase[luid_start..].split_once("_0x")?;
    let (low, rest) = rest.split_once("_phys_")?;
    let (physical, rest) = rest.split_once("_eng_")?;
    let (engine, engine_type) = rest.split_once("_engtype_")?;
    let high = u32::from_str_radix(high, 16).ok()?;
    let low = u32::from_str_radix(low, 16).ok()?;
    Some(Instance {
        luid: canonical_luid(high as i32, low),
        physical: physical.into(),
        engine: engine.into(),
        engine_type: engine_type.into(),
    })
}

fn first_sample(device: &CanonicalGpu) -> ProviderSample {
    ProviderSample {
        unavailable: vec![UnavailableObservation {
            device_id: device.identity.id.clone(),
            metric: MetricKey::UtilizationOverall,
            reason: UnavailableReason::FirstSample,
            source: Some(PROVIDER_ID.into()),
            message: Some("Windows PDH rate counters require two collections".into()),
        }],
        ..ProviderSample::default()
    }
}

fn engine_metrics(
    device_id: &str,
    engines: HashMap<EngineKey, f64>,
    interval_ms: Option<u64>,
) -> ProviderSample {
    let timestamp = now_millis();
    let overall = engines.values().copied().fold(0.0_f64, f64::max);
    let mut values = vec![(
        MetricKey::UtilizationOverall,
        overall,
        "maximum active WDDM engine percentage across this adapter",
    )];
    for (key, needles, definition) in [
        (
            MetricKey::UtilizationGraphics,
            &["3d", "graphics"][..],
            "maximum WDDM graphics engine percentage",
        ),
        (
            MetricKey::UtilizationCompute,
            &["compute"][..],
            "maximum WDDM compute engine percentage",
        ),
        (
            MetricKey::UtilizationCopy,
            &["copy"][..],
            "maximum WDDM copy engine percentage",
        ),
        (
            MetricKey::UtilizationEncoder,
            &["encode"][..],
            "maximum WDDM video encode engine percentage",
        ),
        (
            MetricKey::UtilizationDecoder,
            &["decode"][..],
            "maximum WDDM video decode engine percentage",
        ),
    ] {
        if let Some(value) = engines
            .iter()
            .filter(|(engine, _)| {
                needles
                    .iter()
                    .any(|needle| engine.engine_type.contains(needle))
            })
            .map(|(_, value)| *value)
            .reduce(f64::max)
        {
            values.push((key, value, definition));
        }
    }

    ProviderSample {
        metrics: values
            .into_iter()
            .map(|(metric, value, definition)| MetricObservation {
                device_id: device_id.into(),
                metric,
                value: MetricValue::Number(value),
                source: PROVIDER_ID.into(),
                quality: MetricQuality::Derived,
                sampled_at: timestamp,
                interval_ms,
                definition: Some(definition.into()),
            })
            .collect(),
        ..ProviderSample::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pdh_gpu_engine_instances() {
        let value =
            parse_instance("pid_42_luid_0x00000001_0x0000abcd_phys_0_eng_3_engtype_3D").unwrap();
        assert_eq!(value.luid, "00000001:0000abcd");
        assert_eq!(value.engine, "3");
        assert_eq!(value.engine_type, "3d");
    }

    #[test]
    fn sums_processes_per_engine_then_caps_rounding_noise() {
        let items = vec![
            ("pid_1_luid_0x0_0x1_phys_0_eng_0_engtype_3D".into(), 60.0),
            ("pid_2_luid_0x0_0x1_phys_0_eng_0_engtype_3D".into(), 41.0),
        ];
        let values = aggregate_engines(items, "00000000:00000001");
        assert_eq!(values.values().copied().next(), Some(100.0));
    }
}
