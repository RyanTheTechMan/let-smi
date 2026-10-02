//! Optional read-only Intel telemetry. Inventory remains owned by DXGI/sysfs.
mod api;
#[allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    clippy::all
)]
mod ffi;

use crate::error::{GpuError, Result};
use crate::model::{
    CanonicalGpu, CapabilitySet, DeviceObservation, GpuVendor, Metric, MetricKey,
    MetricObservation, MetricQuality, MetricValue, ProviderDiagnostic, ProviderSample,
    SampleRequest, UnavailableObservation, UnavailableReason, now_millis,
};
use crate::provider::{InventoryProvider, ProviderMetadata, TelemetryProvider};
use api::{Api, check, enumerate, failure};
use ffi::*;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

const ID: &str = "level-zero";
const MAX_DEVICES: u32 = 512;
const MAX_DOMAINS: u32 = 128;

struct Engine {
    handle: usize,
    group: u32,
    on_subdevice: bool,
    subdevice_id: u32,
    baseline: Option<(u64, u64)>,
    observed_at: Option<Instant>,
    last: Option<Metric<f64>>,
}
struct Record {
    engines: Vec<Engine>,
    frequencies: Vec<(usize, MetricKey)>,
    temperatures: Vec<(usize, MetricKey)>,
    power: Option<usize>,
    energy_baseline: Option<(u64, u64)>,
    capabilities: CapabilitySet,
}
struct State {
    api: Option<Arc<Api>>,
    records: BTreeMap<String, Record>,
    failure: Option<GpuError>,
    warnings: Vec<String>,
}

pub struct LevelZeroProvider {
    state: Mutex<State>,
}

impl Default for LevelZeroProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl LevelZeroProvider {
    pub fn new() -> Self {
        let (api, failure) = match Api::load().and_then(|api| {
            // SAFETY: checked function pointer; standalone Sysman initialization
            // requires no environment mutation and creates no owned compute context.
            check(unsafe { (api.init)(0) })?;
            Ok(Arc::new(api))
        }) {
            Ok(api) => (Some(api), None),
            Err(error) => (None, Some(error)),
        };
        Self {
            state: Mutex::new(State {
                api,
                failure,
                records: BTreeMap::new(),
                warnings: Vec::new(),
            }),
        }
    }
}

impl InventoryProvider for LevelZeroProvider {
    fn provider_id(&self) -> &'static str {
        ID
    }
    fn enumerate(&self) -> Result<Vec<DeviceObservation>> {
        let mut state = self.state.lock();
        let Some(api) = state.api.clone() else {
            return Ok(Vec::new());
        };
        state.records.clear();
        state.warnings.clear();
        // SAFETY: enumeration callbacks use valid count and bounded output arrays.
        let result = (|| {
            let mut records = BTreeMap::new();
            let drivers = enumerate(
                |count, items| unsafe { (api.drivers)(count, items.cast()) },
                32,
            )?;
            let mut total = 0;
            for driver in drivers {
                let devices = enumerate(
                    |count, items| unsafe { (api.devices)(driver as _, count, items.cast()) },
                    MAX_DEVICES,
                )?;
                for handle in devices {
                    total += 1;
                    if total > MAX_DEVICES {
                        return Err(failure(
                            UnavailableReason::ProviderError,
                            "Sysman device limit exceeded".into(),
                        ));
                    }
                    match discover(&api, handle, &mut state.warnings) {
                        Ok(Some((pci, record))) => {
                            // Ambiguous PCI identities cannot safely attribute telemetry.
                            if records.contains_key(&pci) {
                                return Err(failure(
                                    UnavailableReason::ProviderError,
                                    format!("duplicate Sysman PCI identity {pci}"),
                                ));
                            }
                            records.insert(pci, record);
                        }
                        Ok(None) => {}
                        Err(error) => warn(&mut state.warnings, error.to_string()),
                    }
                }
            }
            Ok(records)
        })();
        match result {
            Ok(records) => {
                state.records = records;
                state.failure = None;
                Ok(Vec::new())
            }
            Err(error) => {
                state.failure = Some(error.clone());
                Err(error)
            }
        }
    }
    fn diagnostic(&self) -> ProviderDiagnostic {
        let state = self.state.lock();
        ProviderDiagnostic {
            id: ID.into(),
            loaded: state.api.is_some(),
            version: None,
            devices_matched: state.records.len(),
            reason: state.failure.as_ref().map(reason),
            message: state
                .failure
                .as_ref()
                .map(ToString::to_string)
                .or_else(|| (!state.warnings.is_empty()).then(|| state.warnings.join("; "))),
        }
    }
}

impl TelemetryProvider for LevelZeroProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata::new(ID, 100, 90)
            .prefer(MetricKey::UtilizationOverall, 100)
            .prefer(MetricKey::UtilizationGraphics, 100)
            .prefer(MetricKey::UtilizationCompute, 100)
            .prefer(MetricKey::UtilizationCopy, 100)
            .prefer(MetricKey::UtilizationEncoder, 100)
            .prefer(MetricKey::UtilizationDecoder, 100)
            .prefer(MetricKey::ClockGraphicsMhz, 100)
            .prefer(MetricKey::ClockMemoryMhz, 100)
            .prefer(MetricKey::TemperatureCoreCelsius, 100)
            .prefer(MetricKey::TemperatureMemoryCelsius, 100)
            .prefer(MetricKey::PowerDrawWatts, 100)
            .prefer(MetricKey::PowerEnergyJoules, 100)
    }
    fn capabilities(&self, device: &CanonicalGpu) -> CapabilitySet {
        pci_key(device)
            .and_then(|pci| {
                self.state
                    .lock()
                    .records
                    .get(pci)
                    .map(|record| record.capabilities.clone())
            })
            .unwrap_or_default()
    }
    fn sample(&self, device: &CanonicalGpu, request: &SampleRequest) -> Result<ProviderSample> {
        let Some(pci) = pci_key(device) else {
            return Ok(ProviderSample::default());
        };
        let mut state = self.state.lock();
        let Some(api) = state.api.clone() else {
            return Ok(ProviderSample::default());
        };
        let Some(record) = state.records.get_mut(pci) else {
            return Ok(ProviderSample::default());
        };
        Ok(sample_record(&api, record, &device.identity.id, request))
    }
    fn vendor_info(&self, device: &CanonicalGpu) -> Result<serde_json::Value> {
        let state = self.state.lock();
        let Some(record) = pci_key(device).and_then(|pci| state.records.get(pci)) else {
            return Ok(serde_json::Value::Null);
        };
        let mut groups = Vec::with_capacity(record.engines.len());
        for (index, engine) in record.engines.iter().enumerate() {
            let location = if engine.on_subdevice {
                engine.subdevice_id.to_string()
            } else {
                "device".into()
            };
            let mut info = serde_json::json!({
                "name": format!("{}:{location}:{index}", engine_name(engine.group))
            });
            if let Some(value) = &engine.last {
                info["utilization"] = serde_json::to_value(value)
                    .map_err(|error| crate::error::GpuError::Internal(error.to_string()))?;
            }
            groups.push(info);
        }
        Ok(serde_json::json!({ "engineGroups": groups }))
    }

    fn shutdown(&self) {
        let mut state = self.state.lock();
        state.records.clear();
        state.api = None;
        state.failure = Some(failure(
            UnavailableReason::DeviceLost,
            "provider has been closed".into(),
        ));
    }
}

fn pci_key(device: &CanonicalGpu) -> Option<&str> {
    (device.identity.vendor == GpuVendor::Intel).then_some(())?;
    device.identity.pci.as_ref()?.address.as_deref()
}
fn reason(error: &GpuError) -> UnavailableReason {
    match error {
        GpuError::Provider { reason, .. } => *reason,
        _ => UnavailableReason::ProviderError,
    }
}
fn warn(warnings: &mut Vec<String>, message: String) {
    if warnings.len() < 16 {
        warnings.push(message.chars().take(100).collect());
    }
}

fn discover(
    api: &Api,
    handle: usize,
    warnings: &mut Vec<String>,
) -> Result<Option<(String, Record)>> {
    let mut properties = zes_device_properties_t {
        stype: ZES_STRUCTURE_TYPE_DEVICE_PROPERTIES,
        ..Default::default()
    };
    // SAFETY: handle came from Sysman enumeration, output is the pinned ABI structure.
    check(unsafe { (api.device_properties)(handle as _, &raw mut properties) })?;
    if properties.core.vendorId != 0x8086 {
        return Ok(None);
    }
    let mut pci = zes_pci_properties_t {
        stype: ZES_STRUCTURE_TYPE_PCI_PROPERTIES,
        ..Default::default()
    };
    // SAFETY: same enumerated handle and bounded output struct.
    check(unsafe { (api.pci)(handle as _, &raw mut pci) })?;
    let pci = pci_address(pci.address)?;
    let mut record = Record {
        engines: Vec::new(),
        frequencies: Vec::new(),
        temperatures: Vec::new(),
        power: None,
        energy_baseline: None,
        capabilities: CapabilitySet::default(),
    };
    // All callbacks retain the runtime and pass only bounded, properly typed outputs.
    let engines = enumerate(
        |count, items| unsafe { (api.engines)(handle as _, count, items.cast()) },
        MAX_DOMAINS,
    );
    for engine in domains(engines, warnings) {
        let mut props = zes_engine_properties_t {
            stype: ZES_STRUCTURE_TYPE_ENGINE_PROPERTIES,
            ..Default::default()
        };
        let mut stats = zes_engine_stats_t::default();
        // SAFETY: both output records match the enumerated component's C ABI.
        if check(unsafe { (api.engine_properties)(engine as _, &raw mut props) }).is_err() {
            continue;
        }
        if !probe_supported(
            unsafe { (api.activity)(engine as _, &raw mut stats) },
            warnings,
        ) {
            continue;
        }
        record.capabilities.insert(MetricKey::UtilizationOverall);
        if let Some(metric) = engine_metric(props.type_) {
            record.capabilities.insert(metric);
        }
        record.engines.push(Engine {
            handle: engine,
            group: props.type_,
            on_subdevice: props.onSubdevice != 0,
            subdevice_id: props.subdeviceId,
            baseline: None,
            observed_at: None,
            last: None,
        });
    }
    let frequencies = enumerate(
        |count, items| unsafe { (api.frequencies)(handle as _, count, items.cast()) },
        MAX_DOMAINS,
    );
    for frequency in domains(frequencies, warnings) {
        let mut props = zes_freq_properties_t {
            stype: ZES_STRUCTURE_TYPE_FREQ_PROPERTIES,
            ..Default::default()
        };
        let mut value = zes_freq_state_t {
            stype: ZES_STRUCTURE_TYPE_FREQ_STATE,
            ..Default::default()
        };
        // SAFETY: checked domain handle and initialized output records.
        if check(unsafe { (api.frequency_properties)(frequency as _, &raw mut props) }).is_err() {
            continue;
        }
        let metric = match props.type_ {
            ZES_FREQ_DOMAIN_GPU => MetricKey::ClockGraphicsMhz,
            ZES_FREQ_DOMAIN_MEMORY => MetricKey::ClockMemoryMhz,
            _ => continue,
        };
        if !probe_supported(
            unsafe { (api.frequency)(frequency as _, &raw mut value) },
            warnings,
        ) {
            continue;
        }
        record.capabilities.insert(metric);
        record.frequencies.push((frequency, metric));
    }
    let temperatures = enumerate(
        |count, items| unsafe { (api.temperatures)(handle as _, count, items.cast()) },
        MAX_DOMAINS,
    );
    for temperature in domains(temperatures, warnings) {
        let mut props = zes_temp_properties_t {
            stype: ZES_STRUCTURE_TYPE_TEMP_PROPERTIES,
            ..Default::default()
        };
        let mut value = 0.0;
        // SAFETY: checked sensor handle, output struct and f64 storage.
        if check(unsafe { (api.temperature_properties)(temperature as _, &raw mut props) }).is_err()
        {
            continue;
        }
        let metric = match props.type_ {
            ZES_TEMP_SENSORS_GPU => MetricKey::TemperatureCoreCelsius,
            ZES_TEMP_SENSORS_MEMORY => MetricKey::TemperatureMemoryCelsius,
            _ => continue,
        };
        if !probe_supported(
            unsafe { (api.temperature)(temperature as _, &raw mut value) },
            warnings,
        ) {
            continue;
        }
        record.capabilities.insert(metric);
        record.temperatures.push((temperature, metric));
    }
    let powers = enumerate(
        |count, items| unsafe { (api.powers)(handle as _, count, items.cast()) },
        MAX_DOMAINS,
    );
    for power in domains(powers, warnings) {
        let mut limit = zes_power_limit_ext_desc_t {
            stype: ZES_STRUCTURE_TYPE_POWER_LIMIT_EXT_DESC,
            ..Default::default()
        };
        let mut extension = zes_power_ext_properties_t {
            stype: ZES_STRUCTURE_TYPE_POWER_EXT_PROPERTIES,
            defaultLimit: &raw mut limit,
            ..Default::default()
        };
        let mut props = zes_power_properties_t {
            stype: ZES_STRUCTURE_TYPE_POWER_PROPERTIES,
            pNext: (&raw mut extension).cast(),
            ..Default::default()
        };
        // SAFETY: extension chain points to live stack records of the pinned ABI.
        if check(unsafe { (api.power_properties)(power as _, &raw mut props) }).is_err() {
            continue;
        }
        // Only explicit GPU domains or a discrete card can represent GPU power.
        // Bit 0 of ze_device_properties flags is ZE_DEVICE_PROPERTY_FLAG_INTEGRATED.
        let integrated = properties.core.flags & 1 != 0;
        if props.onSubdevice != 0
            || !(extension.domain == ZES_POWER_DOMAIN_GPU
                || (!integrated && extension.domain == ZES_POWER_DOMAIN_CARD))
        {
            continue;
        }
        let mut value = zes_power_energy_counter_t::default();
        if !probe_supported(
            unsafe { (api.energy)(power as _, &raw mut value) },
            warnings,
        ) {
            continue;
        }
        if record.power.is_none() || extension.domain == ZES_POWER_DOMAIN_GPU {
            record.power = Some(power);
        }
    }
    if record.power.is_some() {
        record.capabilities.insert(MetricKey::PowerDrawWatts);
        record.capabilities.insert(MetricKey::PowerEnergyJoules);
    }
    if record.capabilities.metrics().next().is_some() {
        record.capabilities = record.capabilities.with_extension("intel.levelZero");
    }
    Ok(Some((pci, record)))
}

fn pci_address(address: zes_pci_address_t) -> Result<String> {
    if address.domain > 0xffff || address.bus > 0xff || address.device > 31 || address.function > 7
    {
        return Err(failure(
            UnavailableReason::ProviderError,
            "invalid Sysman PCI address".into(),
        ));
    }
    Ok(format!(
        "{:04x}:{:02x}:{:02x}.{}",
        address.domain, address.bus, address.device, address.function
    ))
}
fn domains(result: Result<Vec<usize>>, warnings: &mut Vec<String>) -> Vec<usize> {
    match result {
        Ok(domains) => domains,
        Err(error) => {
            warn(warnings, error.to_string());
            Vec::new()
        }
    }
}
fn probe_supported(code: ze_result_t, warnings: &mut Vec<String>) -> bool {
    match check(code) {
        Ok(()) => true,
        Err(error) => {
            warn(warnings, error.to_string());
            !matches!(
                reason(&error),
                UnavailableReason::Unsupported
                    | UnavailableReason::DeviceLost
                    | UnavailableReason::DriverLibraryMissing
            )
        }
    }
}
fn engine_metric(group: u32) -> Option<MetricKey> {
    match group {
        ZES_ENGINE_GROUP_COMPUTE_ALL | ZES_ENGINE_GROUP_COMPUTE_SINGLE => {
            Some(MetricKey::UtilizationCompute)
        }
        ZES_ENGINE_GROUP_RENDER_ALL
        | ZES_ENGINE_GROUP_3D_ALL
        | ZES_ENGINE_GROUP_RENDER_SINGLE
        | ZES_ENGINE_GROUP_3D_SINGLE => Some(MetricKey::UtilizationGraphics),
        ZES_ENGINE_GROUP_COPY_ALL | ZES_ENGINE_GROUP_COPY_SINGLE => {
            Some(MetricKey::UtilizationCopy)
        }
        ZES_ENGINE_GROUP_MEDIA_ENCODE_SINGLE => Some(MetricKey::UtilizationEncoder),
        ZES_ENGINE_GROUP_MEDIA_DECODE_SINGLE => Some(MetricKey::UtilizationDecoder),
        _ => None,
    }
}
fn engine_name(group: u32) -> &'static str {
    match group {
        ZES_ENGINE_GROUP_ALL => "all",
        ZES_ENGINE_GROUP_COMPUTE_ALL | ZES_ENGINE_GROUP_COMPUTE_SINGLE => "compute",
        ZES_ENGINE_GROUP_MEDIA_ALL => "media",
        ZES_ENGINE_GROUP_COPY_ALL | ZES_ENGINE_GROUP_COPY_SINGLE => "copy",
        ZES_ENGINE_GROUP_RENDER_ALL | ZES_ENGINE_GROUP_RENDER_SINGLE => "render",
        ZES_ENGINE_GROUP_3D_ALL | ZES_ENGINE_GROUP_3D_SINGLE => "3d",
        ZES_ENGINE_GROUP_3D_RENDER_COMPUTE_ALL => "3d-render-compute",
        ZES_ENGINE_GROUP_MEDIA_ENCODE_SINGLE => "encode",
        ZES_ENGINE_GROUP_MEDIA_DECODE_SINGLE => "decode",
        ZES_ENGINE_GROUP_MEDIA_CODEC_SINGLE => "media-codec",
        _ => "other",
    }
}

/// Sysman active/energy counters and timestamps are unsigned monotonic values.
/// A reset establishes a new baseline rather than manufacturing a spike.
fn counter_delta(baseline: &mut Option<(u64, u64)>, current: (u64, u64)) -> Result<(u64, u64)> {
    let previous = baseline.replace(current).ok_or_else(|| {
        failure(
            UnavailableReason::FirstSample,
            "counter baseline is not established".into(),
        )
    })?;
    let (Some(delta), Some(elapsed)) = (
        current.0.checked_sub(previous.0),
        current.1.checked_sub(previous.1),
    ) else {
        return Err(failure(
            UnavailableReason::FirstSample,
            "counter reset; baseline re-established".into(),
        ));
    };
    if elapsed == 0 {
        return Err(failure(
            UnavailableReason::TemporarilyUnavailable,
            "counter timestamp did not advance".into(),
        ));
    }
    Ok((delta, elapsed))
}

fn sample_record(
    api: &Api,
    record: &mut Record,
    id: &str,
    request: &SampleRequest,
) -> ProviderSample {
    let mut sample = ProviderSample::default();
    let mut engine_values = Vec::new();
    for engine in &mut record.engines {
        let mut stats = zes_engine_stats_t::default();
        // SAFETY: retained runtime and enumerated engine handle with bounded output.
        let result =
            check(unsafe { (api.activity)(engine.handle as _, &raw mut stats) }).and_then(|()| {
                // Engine timestamp units are implementation-specific. Use their
                // ratio for occupancy, and our observation clock for milliseconds.
                let observed_at = Instant::now();
                let previous_observation = engine.observed_at.replace(observed_at);
                let (active, elapsed) =
                    counter_delta(&mut engine.baseline, (stats.activeTime, stats.timestamp))?;
                let percent = active as f64 / elapsed as f64 * 100.0;
                if !percent.is_finite() || percent > 100.0 + 1e-6 {
                    return Err(failure(
                        UnavailableReason::ProviderError,
                        "engine activity exceeds its measured interval".into(),
                    ));
                }
                let interval_ms = previous_observation.map_or(0, |previous| {
                    u64::try_from(observed_at.duration_since(previous).as_millis())
                        .unwrap_or(u64::MAX)
                });
                Ok((percent.min(100.0), interval_ms))
            });
        if result
            .as_ref()
            .is_err_and(|error| reason(error) == UnavailableReason::DeviceLost)
        {
            engine.baseline = None;
            engine.observed_at = None;
        }
        engine.last = Some(match &result {
            Ok((value, interval)) => Metric::available(
                *value,
                ID.into(),
                MetricQuality::Derived,
                now_millis(),
                Some(*interval),
                Some(
                    "Intel Sysman engine-group active-time delta divided by its timestamp delta; interval measures monotonic observation time"
                        .into(),
                ),
            ),
            Err(error) => {
                Metric::unavailable(reason(error), Some(ID.into()), Some(error.to_string()))
            }
        });
        engine_values.push((engine.group, engine.on_subdevice, result));
    }
    for metric in [
        MetricKey::UtilizationOverall,
        MetricKey::UtilizationGraphics,
        MetricKey::UtilizationCompute,
        MetricKey::UtilizationCopy,
        MetricKey::UtilizationEncoder,
        MetricKey::UtilizationDecoder,
    ] {
        if !record.capabilities.supports(metric) || !request.wants(metric) {
            continue;
        }
        let aggregate: Vec<_> = engine_values
            .iter()
            .filter(|(group, subdevice, _)| {
                !subdevice
                    && if metric == MetricKey::UtilizationOverall {
                        *group == ZES_ENGINE_GROUP_ALL
                    } else {
                        engine_metric(*group) == Some(metric)
                            && matches!(
                                *group,
                                ZES_ENGINE_GROUP_COMPUTE_ALL
                                    | ZES_ENGINE_GROUP_RENDER_ALL
                                    | ZES_ENGINE_GROUP_3D_ALL
                                    | ZES_ENGINE_GROUP_COPY_ALL
                            )
                    }
            })
            .collect();
        let values: Vec<_> = if aggregate.is_empty() {
            engine_values
                .iter()
                .filter(|(group, _, _)| {
                    metric == MetricKey::UtilizationOverall || engine_metric(*group) == Some(metric)
                })
                .collect()
        } else {
            aggregate
        };
        let definition = if metric == MetricKey::UtilizationOverall
            && values.len() == 1
            && values[0].0 == ZES_ENGINE_GROUP_ALL
            && !values[0].1
        {
            "Intel Sysman device-wide all-engine active residency"
        } else {
            "maximum measured Intel Sysman engine-group occupancy; engine groups are not summed"
        };
        emit_max(
            &mut sample,
            id,
            metric,
            values.iter().map(|(_, _, value)| value.clone()),
            MetricQuality::Derived,
            definition,
        );
    }
    for (handles, temperature) in [(&record.frequencies, false), (&record.temperatures, true)] {
        let mut readings: BTreeMap<MetricKey, Vec<Result<(f64, u64)>>> = BTreeMap::new();
        for (handle, metric) in handles {
            if !request.wants(*metric) {
                continue;
            }
            let value = if temperature {
                let mut value = 0.0;
                // SAFETY: retained sensor and writable f64.
                check(unsafe { (api.temperature)(*handle as _, &raw mut value) }).map(|()| value)
            } else {
                let mut value = zes_freq_state_t {
                    stype: ZES_STRUCTURE_TYPE_FREQ_STATE,
                    ..Default::default()
                };
                // SAFETY: retained frequency handle and initialized ABI output.
                check(unsafe { (api.frequency)(*handle as _, &raw mut value) })
                    .map(|()| value.actual)
            }
            .and_then(|value| {
                if !value.is_finite()
                    || (!temperature && value < 0.0)
                    || (temperature && !(-273.15..=250.0).contains(&value))
                {
                    Err(failure(
                        UnavailableReason::ProviderError,
                        "invalid sensor reading".into(),
                    ))
                } else {
                    Ok((value, 0))
                }
            });
            readings.entry(*metric).or_default().push(value);
        }
        for (metric, values) in readings {
            let quality = if values.len() == 1 {
                MetricQuality::Direct
            } else {
                MetricQuality::Derived
            };
            emit_max(
                &mut sample,
                id,
                metric,
                values,
                quality,
                if temperature {
                    "maximum attributable Intel Sysman GPU or memory temperature"
                } else {
                    "maximum actual Intel Sysman domain frequency in MHz"
                },
            );
        }
    }
    if let Some(handle) = record.power {
        let mut value = zes_power_energy_counter_t::default();
        // SAFETY: retained GPU-specific power handle and ABI counter record.
        let result = check(unsafe { (api.energy)(handle as _, &raw mut value) });
        if request.wants(MetricKey::PowerEnergyJoules) {
            emit_max(
                &mut sample,
                id,
                MetricKey::PowerEnergyJoules,
                [result
                    .clone()
                    .map(|()| (value.energy as f64 / 1_000_000.0, 0))],
                MetricQuality::Direct,
                "cumulative energy of an explicitly identified Intel GPU/card power domain in joules",
            );
        }
        let power = result
            .and_then(|()| {
                counter_delta(&mut record.energy_baseline, (value.energy, value.timestamp))
            })
            .map(|(energy, elapsed)| (energy as f64 / elapsed as f64, elapsed / 1_000));
        if power
            .as_ref()
            .is_err_and(|error| reason(error) == UnavailableReason::DeviceLost)
        {
            record.energy_baseline = None;
        }
        if request.wants(MetricKey::PowerDrawWatts) {
            emit_max(
                &mut sample,
                id,
                MetricKey::PowerDrawWatts,
                [power],
                MetricQuality::Derived,
                "GPU/card energy delta divided by its timestamp delta in watts",
            );
        }
    }
    sample
}

fn emit_max(
    sample: &mut ProviderSample,
    id: &str,
    metric: MetricKey,
    values: impl IntoIterator<Item = Result<(f64, u64)>>,
    quality: MetricQuality,
    definition: &str,
) {
    let mut best: Option<(f64, u64)> = None;
    let mut error = None;
    for value in values {
        match value {
            Ok(value) if best.is_none_or(|best| value.0 > best.0) => best = Some(value),
            Ok(_) => {}
            Err(value) => error = Some(value),
        }
    }
    if let Some((value, interval)) = best {
        sample.metrics.push(MetricObservation {
            device_id: id.into(),
            metric,
            value: MetricValue::Number(value),
            source: ID.into(),
            quality,
            sampled_at: now_millis(),
            interval_ms: (quality == MetricQuality::Derived && interval > 0).then_some(interval),
            definition: Some(definition.into()),
        });
    } else if let Some(error) = error {
        sample.unavailable.push(UnavailableObservation {
            device_id: id.into(),
            metric,
            reason: reason(&error),
            source: Some(ID.into()),
            message: Some(error.to_string()),
        });
    }
}

#[cfg(test)]
mod tests;
