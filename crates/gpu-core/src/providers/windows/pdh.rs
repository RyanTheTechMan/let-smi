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
use std::mem::{align_of, size_of, zeroed};
use std::time::{Duration, Instant};
use windows::Win32::System::Performance::{
    PDH_ACCESS_DENIED, PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W,
    PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery,
    PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows::core::{PCWSTR, w};

const PROVIDER_ID: &str = "windows-pdh";
const GPU_ENGINE_COUNTER: PCWSTR = w!("\\GPU Engine(*)\\Utilization Percentage");
const MAX_PDH_BUFFER_BYTES: usize = 16 * 1024 * 1024;
const MAX_PDH_ITEMS: usize = 16_384;
const MAX_PDH_NAME_UNITS: usize = 512;
const MAX_PDH_MORE_DATA_RETRIES: usize = 3;
const EXPECTED_PDH_ITEM_SIZE_64: usize = 24;
const PDH_COLLECTION_COALESCE: Duration = Duration::from_millis(10);

#[derive(Clone)]
enum PdhCollection {
    FirstSample,
    Items {
        values: Vec<(String, f64)>,
        interval_ms: Option<u64>,
    },
}

struct QueryState {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    primed: bool,
    last_collected: Option<Instant>,
    cached: Option<(Instant, PdhCollection)>,
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
    Unavailable {
        reason: UnavailableReason,
        message: String,
    },
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
            State::Unavailable { reason, message } => ProviderDiagnostic {
                id: PROVIDER_ID.into(),
                loaded: false,
                version: None,
                devices_matched: 0,
                reason: Some(*reason),
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

        let collection = collect_or_reuse(query)?;
        let (items, interval_ms) = match collection {
            PdhCollection::FirstSample => return Ok(first_sample(device)),
            PdhCollection::Items {
                values,
                interval_ms,
            } => (values, interval_ms),
        };
        let engines = aggregate_engines(items, luid);
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
        return unavailable_state(status, format!("PdhOpenQueryW failed with 0x{status:08x}"));
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
        return unavailable_state(
            status,
            format!("GPU Engine counters are unavailable (0x{status:08x})"),
        );
    }
    State::Ready(QueryState {
        query,
        counter,
        primed: false,
        last_collected: None,
        cached: None,
    })
}

fn unavailable_state(status: u32, message: String) -> State {
    State::Unavailable {
        reason: if status == PDH_ACCESS_DENIED {
            UnavailableReason::PermissionDenied
        } else {
            UnavailableReason::Unsupported
        },
        message,
    }
}

fn collect_or_reuse(query: &mut QueryState) -> Result<PdhCollection> {
    let now = Instant::now();
    if let Some((cached_at, collection)) = &query.cached
        && now.saturating_duration_since(*cached_at) <= PDH_COLLECTION_COALESCE
    {
        return Ok(collection.clone());
    }

    // SAFETY: the query handle is live and serialized by the provider mutex.
    let status = unsafe { PdhCollectQueryData(query.query) };
    if status != 0 {
        return Err(pdh_api_error(
            status,
            format!("PdhCollectQueryData failed with 0x{status:08x}"),
        ));
    }
    let collected_at = Instant::now();
    let interval_ms = query.last_collected.replace(collected_at).map(|previous| {
        u64::try_from(collected_at.saturating_duration_since(previous).as_millis())
            .unwrap_or(u64::MAX)
    });
    let collection = if query.primed {
        PdhCollection::Items {
            values: formatted_items(query.counter)?,
            interval_ms,
        }
    } else {
        query.primed = true;
        PdhCollection::FirstSample
    };
    query.cached = Some((collected_at, collection.clone()));
    Ok(collection)
}

fn formatted_items(counter: PDH_HCOUNTER) -> Result<Vec<(String, f64)>> {
    validate_pdh_item_abi().map_err(pdh_layout_error)?;

    for _attempt in 0..MAX_PDH_MORE_DATA_RETRIES {
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
            return Err(pdh_api_error(
                status,
                format!("PDH counter size query failed with 0x{status:08x}"),
            ));
        }
        if byte_count == 0 && item_count == 0 {
            return Ok(Vec::new());
        }

        let requested_bytes = usize::try_from(byte_count)
            .map_err(|_| pdh_layout_error("PDH byte count does not fit usize"))?;
        let requested_items = usize::try_from(item_count)
            .map_err(|_| pdh_layout_error("PDH item count does not fit usize"))?;
        validate_pdh_layout(requested_bytes, requested_items, requested_bytes)
            .map_err(pdh_layout_error)?;

        let words = requested_bytes
            .checked_add(size_of::<usize>() - 1)
            .ok_or_else(|| pdh_layout_error("PDH allocation rounding overflowed"))?
            / size_of::<usize>();
        let capacity_bytes = words
            .checked_mul(size_of::<usize>())
            .ok_or_else(|| pdh_layout_error("PDH allocation size overflowed"))?;
        let mut buffer = vec![0_usize; words];
        let mut actual_bytes = u32::try_from(capacity_bytes)
            .map_err(|_| pdh_layout_error("PDH allocation exceeds the API byte-count range"))?;
        let mut actual_items = item_count;
        // SAFETY: the bounded usize allocation is sufficiently sized and
        // aligned for the item array, and both counts are writable parameters.
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                counter,
                PDH_FMT_DOUBLE,
                &raw mut actual_bytes,
                &raw mut actual_items,
                Some(buffer.as_mut_ptr().cast()),
            )
        };
        if status == PDH_MORE_DATA {
            continue;
        }
        if status != 0 {
            return Err(pdh_api_error(
                status,
                format!("PDH counter read failed with 0x{status:08x}"),
            ));
        }

        let actual_bytes = usize::try_from(actual_bytes)
            .map_err(|_| pdh_layout_error("returned PDH byte count does not fit usize"))?;
        let actual_items = usize::try_from(actual_items)
            .map_err(|_| pdh_layout_error("returned PDH item count does not fit usize"))?;
        let item_bytes = validate_pdh_layout(actual_bytes, actual_items, capacity_bytes)
            .map_err(pdh_layout_error)?;

        // SAFETY: validate_pdh_layout proves the contiguous item array fits in
        // the initialized, correctly aligned allocation.
        let items = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(),
                actual_items,
            )
        };
        let buffer_start = buffer.as_ptr() as usize;
        let mut result = Vec::with_capacity(actual_items);
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
            let name = read_bounded_name(
                buffer_start,
                actual_bytes,
                item_bytes,
                item.szName.0 as usize,
            )
            .map_err(pdh_layout_error)?;
            result.push((name, value));
        }
        return Ok(result);
    }

    Err(GpuError::provider(
        PROVIDER_ID,
        UnavailableReason::ProviderError,
        format!("PDH counter array changed across {MAX_PDH_MORE_DATA_RETRIES} bounded retries"),
    ))
}

fn pdh_layout_error(message: impl Into<String>) -> GpuError {
    GpuError::provider(
        PROVIDER_ID,
        UnavailableReason::ProviderError,
        message.into(),
    )
}

fn pdh_api_error(status: u32, message: String) -> GpuError {
    GpuError::provider(
        PROVIDER_ID,
        if status == PDH_ACCESS_DENIED {
            UnavailableReason::PermissionDenied
        } else {
            UnavailableReason::TemporarilyUnavailable
        },
        message,
    )
}

fn validate_pdh_item_abi() -> std::result::Result<(), String> {
    let item_size = size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    if size_of::<usize>() != 8 || item_size != EXPECTED_PDH_ITEM_SIZE_64 {
        return Err(format!(
            "unexpected PDH item ABI: pointer size {}, item size {item_size}",
            size_of::<usize>()
        ));
    }
    if align_of::<PDH_FMT_COUNTERVALUE_ITEM_W>() > align_of::<usize>() {
        return Err("PDH item alignment exceeds the allocation alignment".into());
    }
    Ok(())
}

fn checked_item_bytes(item_count: usize, item_size: usize) -> std::result::Result<usize, String> {
    item_count
        .checked_mul(item_size)
        .ok_or_else(|| "PDH item count and structure size overflowed".into())
}

fn validate_pdh_layout(
    byte_count: usize,
    item_count: usize,
    buffer_capacity: usize,
) -> std::result::Result<usize, String> {
    if byte_count == 0 || item_count == 0 {
        return Err("PDH returned inconsistent zero byte/item counts".into());
    }
    if byte_count > MAX_PDH_BUFFER_BYTES {
        return Err(format!(
            "PDH byte count {byte_count} exceeds the {MAX_PDH_BUFFER_BYTES}-byte limit"
        ));
    }
    if item_count > MAX_PDH_ITEMS {
        return Err(format!(
            "PDH item count {item_count} exceeds the {MAX_PDH_ITEMS}-item limit"
        ));
    }
    let item_bytes = checked_item_bytes(item_count, size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>())?;
    if byte_count > buffer_capacity {
        return Err("PDH returned more bytes than the allocated buffer".into());
    }
    if item_bytes > byte_count {
        return Err(format!(
            "PDH item array needs {item_bytes} bytes but the returned buffer has {byte_count}"
        ));
    }
    Ok(item_bytes)
}

fn validate_name_pointer(
    buffer_start: usize,
    byte_count: usize,
    item_bytes: usize,
    name_pointer: usize,
) -> std::result::Result<usize, String> {
    let buffer_end = buffer_start
        .checked_add(byte_count)
        .ok_or_else(|| "PDH buffer address overflowed".to_owned())?;
    let names_start = buffer_start
        .checked_add(item_bytes)
        .ok_or_else(|| "PDH item-region address overflowed".to_owned())?;
    if name_pointer < names_start || name_pointer >= buffer_end {
        return Err("PDH counter name pointer lies outside the returned name region".into());
    }
    if name_pointer % align_of::<u16>() != 0 {
        return Err("PDH counter name pointer is not UTF-16 aligned".into());
    }
    let remaining_bytes = buffer_end - name_pointer;
    if remaining_bytes < size_of::<u16>() {
        return Err("PDH counter name has no room for a terminator".into());
    }
    Ok(remaining_bytes / size_of::<u16>())
}

fn read_bounded_name(
    buffer_start: usize,
    byte_count: usize,
    item_bytes: usize,
    name_pointer: usize,
) -> std::result::Result<String, String> {
    let max_units = validate_name_pointer(buffer_start, byte_count, item_bytes, name_pointer)?;
    let scan_units = max_units.min(MAX_PDH_NAME_UNITS.saturating_add(1));
    // SAFETY: validate_name_pointer proves this bounded UTF-16 view remains
    // inside the live PDH allocation and has suitable alignment. scan_units is
    // additionally capped so overlapping malicious pointers cannot amplify one
    // bounded native buffer into unbounded Rust string allocations.
    let units = unsafe { std::slice::from_raw_parts(name_pointer as *const u16, scan_units) };
    let length = bounded_name_length(units, max_units)?;
    String::from_utf16(&units[..length])
        .map_err(|_| "PDH counter name contains invalid UTF-16".to_owned())
}

fn bounded_name_length(
    units: &[u16],
    available_units: usize,
) -> std::result::Result<usize, String> {
    match units.iter().position(|unit| *unit == 0) {
        Some(length) if length <= MAX_PDH_NAME_UNITS => Ok(length),
        Some(_) => Err(format!(
            "PDH counter name exceeds the {MAX_PDH_NAME_UNITS}-unit limit"
        )),
        None if available_units > MAX_PDH_NAME_UNITS => Err(format!(
            "PDH counter name exceeds the {MAX_PDH_NAME_UNITS}-unit limit"
        )),
        None => Err("PDH counter name is not terminated inside the returned buffer".into()),
    }
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
        unavailable: [
            MetricKey::UtilizationOverall,
            MetricKey::UtilizationGraphics,
            MetricKey::UtilizationCompute,
            MetricKey::UtilizationCopy,
            MetricKey::UtilizationEncoder,
            MetricKey::UtilizationDecoder,
        ]
        .into_iter()
        .map(|metric| UnavailableObservation {
            device_id: device.identity.id.clone(),
            metric,
            reason: UnavailableReason::FirstSample,
            source: Some(PROVIDER_ID.into()),
            message: Some("Windows PDH rate counters require two collections".into()),
        })
        .collect(),
        ..ProviderSample::default()
    }
}

fn engine_metrics(
    device_id: &str,
    engines: HashMap<EngineKey, f64>,
    interval_ms: Option<u64>,
) -> ProviderSample {
    if engines.is_empty() {
        return ProviderSample {
            unavailable: pdh_rate_metrics()
                .into_iter()
                .map(|metric| UnavailableObservation {
                    device_id: device_id.into(),
                    metric,
                    reason: UnavailableReason::TemporarilyUnavailable,
                    source: Some(PROVIDER_ID.into()),
                    message: Some("PDH returned no GPU engine counters for this adapter".into()),
                })
                .collect(),
            ..ProviderSample::default()
        };
    }

    let timestamp = now_millis();
    let overall = engines.values().copied().fold(0.0_f64, f64::max);
    let mut values = vec![(
        MetricKey::UtilizationOverall,
        overall,
        "maximum active WDDM engine percentage across this adapter",
    )];
    let mut unavailable = Vec::new();
    for (key, needles, definition, label) in [
        (
            MetricKey::UtilizationGraphics,
            &["3d", "graphics"][..],
            "maximum WDDM graphics engine percentage",
            "graphics",
        ),
        (
            MetricKey::UtilizationCompute,
            &["compute"][..],
            "maximum WDDM compute engine percentage",
            "compute",
        ),
        (
            MetricKey::UtilizationCopy,
            &["copy"][..],
            "maximum WDDM copy engine percentage",
            "copy",
        ),
        (
            MetricKey::UtilizationEncoder,
            &["encode"][..],
            "maximum WDDM video encode engine percentage",
            "video encode",
        ),
        (
            MetricKey::UtilizationDecoder,
            &["decode"][..],
            "maximum WDDM video decode engine percentage",
            "video decode",
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
        } else {
            unavailable.push(UnavailableObservation {
                device_id: device_id.into(),
                metric: key,
                reason: UnavailableReason::TemporarilyUnavailable,
                source: Some(PROVIDER_ID.into()),
                message: Some(format!(
                    "PDH returned no matching WDDM {label} engine counter for this adapter"
                )),
            });
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
        unavailable,
        ..ProviderSample::default()
    }
}

const fn pdh_rate_metrics() -> [MetricKey; 6] {
    [
        MetricKey::UtilizationOverall,
        MetricKey::UtilizationGraphics,
        MetricKey::UtilizationCompute,
        MetricKey::UtilizationCopy,
        MetricKey::UtilizationEncoder,
        MetricKey::UtilizationDecoder,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::GpuVendor;

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

    #[test]
    fn rejects_pdh_count_size_overflow() {
        assert!(checked_item_bytes(usize::MAX, 2).is_err());
    }

    #[test]
    fn rejects_oversized_and_truncated_pdh_buffers() {
        assert!(validate_pdh_layout(MAX_PDH_BUFFER_BYTES + 1, 1, usize::MAX).is_err());
        assert!(validate_pdh_layout(64, MAX_PDH_ITEMS + 1, 64).is_err());
        assert!(validate_pdh_layout(8, 1, 8).is_err());
        assert!(validate_pdh_layout(64, 1, 32).is_err());
        assert!(validate_pdh_layout(0, 1, 1).is_err());
        assert!(validate_pdh_layout(1, 0, 1).is_err());
    }

    #[test]
    fn rejects_malformed_embedded_name_pointers() {
        let start = 0x1_000_usize;
        let bytes = 0x100_usize;
        let item_bytes = 0x30_usize;
        assert!(validate_name_pointer(start, bytes, item_bytes, 0).is_err());
        assert!(validate_name_pointer(start, bytes, item_bytes, start + item_bytes - 2).is_err());
        assert!(validate_name_pointer(start, bytes, item_bytes, start + bytes).is_err());
        assert!(validate_name_pointer(start, bytes, item_bytes, start + item_bytes + 1).is_err());
        assert_eq!(
            validate_name_pointer(start, bytes, item_bytes, start + item_bytes),
            Ok((bytes - item_bytes) / size_of::<u16>())
        );
    }

    #[test]
    fn bounds_counter_name_scans_and_allocations() {
        let mut maximum = vec![b'a' as u16; MAX_PDH_NAME_UNITS];
        maximum.push(0);
        assert_eq!(
            bounded_name_length(&maximum, maximum.len()),
            Ok(MAX_PDH_NAME_UNITS)
        );

        let oversized = vec![b'a' as u16; MAX_PDH_NAME_UNITS + 1];
        assert!(bounded_name_length(&oversized, oversized.len()).is_err());
        assert!(bounded_name_length(&[b'a' as u16], 1).is_err());
    }

    #[test]
    fn pdh_retry_count_and_abi_are_bounded() {
        assert_eq!(MAX_PDH_MORE_DATA_RETRIES, 3);
        assert_eq!(PDH_COLLECTION_COALESCE, Duration::from_millis(10));
        validate_pdh_item_abi().expect("the supported Windows 64-bit PDH ABI must match");
    }

    #[test]
    fn pdh_permission_failures_are_not_reported_as_unsupported() {
        assert!(matches!(
            unavailable_state(PDH_ACCESS_DENIED, "denied".into()),
            State::Unavailable {
                reason: UnavailableReason::PermissionDenied,
                ..
            }
        ));
        assert!(matches!(
            unavailable_state(1, "missing".into()),
            State::Unavailable {
                reason: UnavailableReason::Unsupported,
                ..
            }
        ));
        assert!(matches!(
            pdh_api_error(PDH_ACCESS_DENIED, "denied".into()),
            GpuError::Provider {
                reason: UnavailableReason::PermissionDenied,
                ..
            }
        ));
    }

    #[test]
    fn first_sample_marks_every_pdh_rate_field_unavailable() {
        let mut observation =
            DeviceObservation::new(PROVIDER_ID, "adapter", GpuVendor::Intel, "Intel GPU");
        observation.uuid = Some("fixture".into());
        let device = crate::correlation::correlate(vec![observation])
            .pop()
            .expect("fixture GPU");
        let sample = first_sample(&device);
        assert_eq!(sample.unavailable.len(), 6);
        assert!(
            sample
                .unavailable
                .iter()
                .all(|value| value.reason == UnavailableReason::FirstSample)
        );
    }

    #[test]
    fn absent_engine_groups_are_explicitly_unavailable_not_zero() {
        let engines = HashMap::from([(
            EngineKey {
                physical: "0".into(),
                engine: "0".into(),
                engine_type: "3d".into(),
            },
            0.0,
        )]);
        let sample = engine_metrics("gpu", engines, Some(1_000));
        let overall = sample
            .metrics
            .iter()
            .find(|metric| metric.metric == MetricKey::UtilizationOverall)
            .expect("overall metric");
        assert_eq!(overall.value.as_f64(), Some(0.0));
        assert!(sample.unavailable.iter().any(|value| {
            value.metric == MetricKey::UtilizationCompute
                && value.reason == UnavailableReason::TemporarilyUnavailable
                && value.source.as_deref() == Some(PROVIDER_ID)
        }));
    }
}
