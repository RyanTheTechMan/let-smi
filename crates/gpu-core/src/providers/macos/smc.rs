//! Best-effort Apple SMC GPU temperature provider.
//!
//! The IOKit entry points used here are public system APIs. The AppleSMC user
//! client protocol and sensor key names are undocumented, so every key is
//! discovered and type-checked at runtime and failure never affects Metal
//! inventory.

use crate::error::Result;
use crate::model::{
    CanonicalGpu, CapabilitySet, DeviceObservation, GpuVendor, MacosIdentity, MetricKey,
    MetricObservation, MetricQuality, MetricValue, ProviderDiagnostic, ProviderSample,
    SampleRequest, UnavailableObservation, UnavailableReason, now_millis,
};
use crate::provider::{InventoryProvider, ProviderMetadata, TelemetryProvider};
use metal::Device;
use parking_lot::Mutex;
use std::ffi::{c_char, c_void};
use std::mem::size_of;

const PROVIDER_ID: &str = "apple-smc";
const SMC_SELECTOR: u32 = 2;
const SMC_READ_KEY: u8 = 5;
const SMC_READ_INDEX: u8 = 8;
const SMC_READ_KEY_INFO: u8 = 9;
const SMC_KEY_NOT_FOUND: u8 = 0x84;
const MAX_SMC_KEYS: u32 = 16_384;
const MAX_GPU_TEMPERATURE_KEYS: usize = 64;
const TYPE_FLT: u32 = u32::from_be_bytes(*b"flt ");
const TYPE_SP78: u32 = u32::from_be_bytes(*b"sp78");

type IoObject = u32;
type IoConnect = u32;
type KernReturn = i32;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn mach_task_self() -> u32;
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> IoObject;
    fn IOServiceOpen(
        service: IoObject,
        owning_task: u32,
        connection_type: u32,
        connection: *mut IoConnect,
    ) -> KernReturn;
    fn IOServiceClose(connection: IoConnect) -> KernReturn;
    fn IOObjectRelease(object: IoObject) -> KernReturn;
    fn IOConnectCallStructMethod(
        connection: IoConnect,
        selector: u32,
        input: *const c_void,
        input_size: usize,
        output: *mut c_void,
        output_size: *mut usize,
    ) -> KernReturn;
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct KeyDataVersion {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct PowerLimitData {
    version: u16,
    length: u16,
    cpu_limit: u32,
    gpu_limit: u32,
    memory_limit: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
struct KeyInfo {
    data_size: u32,
    data_type: u32,
    attributes: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct KeyData {
    key: u32,
    version: KeyDataVersion,
    power_limit: PowerLimitData,
    key_info: KeyInfo,
    result: u8,
    status: u8,
    command: u8,
    data32: u32,
    bytes: [u8; 32],
}

const _: [(); 12] = [(); size_of::<KeyInfo>()];
const _: [(); 80] = [(); size_of::<KeyData>()];

#[derive(Debug, Clone)]
struct SmcFailure {
    reason: UnavailableReason,
    message: String,
}

impl SmcFailure {
    fn provider(message: impl Into<String>) -> Self {
        Self {
            reason: UnavailableReason::ProviderError,
            message: message.into(),
        }
    }

    fn temporary(message: impl Into<String>) -> Self {
        Self {
            reason: UnavailableReason::TemporarilyUnavailable,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TemperatureSensor {
    key: u32,
    info: KeyInfo,
}

struct SmcConnection {
    connection: IoConnect,
}

impl SmcConnection {
    fn open() -> std::result::Result<Self, SmcFailure> {
        // SAFETY: IOServiceMatching copies the static C string into an
        // OS-owned matching dictionary consumed by GetMatchingService.
        let matching = unsafe { IOServiceMatching(c"AppleSMC".as_ptr()) };
        if matching.is_null() {
            return Err(SmcFailure::temporary(
                "IOKit could not create an AppleSMC matching dictionary",
            ));
        }
        // SAFETY: matching is consumed by this call as documented by IOKit.
        let service = unsafe { IOServiceGetMatchingService(0, matching) };
        if service == 0 {
            return Err(SmcFailure::temporary(
                "the AppleSMC IOKit service was not found",
            ));
        }

        let mut connection = 0;
        // SAFETY: service is a retained IOKit service and connection points to
        // writable storage. mach_task_self returns the current task port.
        let status = unsafe { IOServiceOpen(service, mach_task_self(), 0, &raw mut connection) };
        // SAFETY: GetMatchingService returned a +1 reference, independent of
        // the user-client connection.
        let _ = unsafe { IOObjectRelease(service) };
        if status != 0 || connection == 0 {
            return Err(SmcFailure::provider(format!(
                "IOServiceOpen(AppleSMC) failed with IOKit status {status:#x}"
            )));
        }
        Ok(Self { connection })
    }

    fn call(&self, input: &KeyData) -> std::result::Result<KeyData, SmcFailure> {
        let mut output = KeyData::default();
        let mut output_size = size_of::<KeyData>();
        // SAFETY: both structures have the validated AppleSMC C layout and
        // remain alive for the duration of the synchronous IOKit call.
        let status = unsafe {
            IOConnectCallStructMethod(
                self.connection,
                SMC_SELECTOR,
                (input as *const KeyData).cast(),
                size_of::<KeyData>(),
                (&raw mut output).cast(),
                &raw mut output_size,
            )
        };
        if status != 0 {
            return Err(SmcFailure::provider(format!(
                "AppleSMC request failed with IOKit status {status:#x}"
            )));
        }
        if output_size != size_of::<KeyData>() {
            return Err(SmcFailure::provider(format!(
                "AppleSMC returned an unexpected payload size ({output_size})"
            )));
        }
        if output.result == SMC_KEY_NOT_FOUND {
            return Err(SmcFailure::temporary("AppleSMC key was not found"));
        }
        if output.result != 0 {
            return Err(SmcFailure::provider(format!(
                "AppleSMC returned result code {:#x}",
                output.result
            )));
        }
        Ok(output)
    }

    fn key_info(&self, key: u32) -> std::result::Result<KeyInfo, SmcFailure> {
        let output = self.call(&KeyData {
            key,
            command: SMC_READ_KEY_INFO,
            ..KeyData::default()
        })?;
        if output.key_info.data_size == 0 || output.key_info.data_size > 32 {
            return Err(SmcFailure::provider(format!(
                "AppleSMC key {} reported invalid size {}",
                fourcc_string(key),
                output.key_info.data_size
            )));
        }
        Ok(output.key_info)
    }

    fn value(&self, sensor: TemperatureSensor) -> std::result::Result<f64, SmcFailure> {
        let output = self.call(&KeyData {
            key: sensor.key,
            key_info: sensor.info,
            command: SMC_READ_KEY,
            ..KeyData::default()
        })?;
        let value = decode_temperature(sensor.info, &output.bytes)?;
        if !(1.0..=150.0).contains(&value) {
            return Err(SmcFailure::provider(format!(
                "AppleSMC key {} returned implausible temperature {value}",
                fourcc_string(sensor.key)
            )));
        }
        Ok(value)
    }

    fn key_count(&self) -> std::result::Result<u32, SmcFailure> {
        let key = fourcc(*b"#KEY");
        let info = self.key_info(key)?;
        if info.data_size < 4 {
            return Err(SmcFailure::provider(
                "AppleSMC #KEY payload was shorter than four bytes",
            ));
        }
        let output = self.call(&KeyData {
            key,
            key_info: info,
            command: SMC_READ_KEY,
            ..KeyData::default()
        })?;
        let count = u32::from_be_bytes(output.bytes[..4].try_into().unwrap_or_default());
        if count > MAX_SMC_KEYS {
            return Err(SmcFailure::provider(format!(
                "AppleSMC reported an unreasonable key count ({count})"
            )));
        }
        Ok(count)
    }

    fn key_at(&self, index: u32) -> std::result::Result<u32, SmcFailure> {
        self.call(&KeyData {
            command: SMC_READ_INDEX,
            data32: index,
            ..KeyData::default()
        })
        .map(|output| output.key)
    }

    fn discover_gpu_temperature_sensors(&self) -> (Vec<TemperatureSensor>, Option<String>) {
        let mut sensors = Vec::new();
        let mut warning = None;
        match self.key_count() {
            Ok(count) => {
                for index in 0..count {
                    if sensors.len() >= MAX_GPU_TEMPERATURE_KEYS {
                        break;
                    }
                    let Ok(key) = self.key_at(index) else {
                        continue;
                    };
                    let Ok(info) = self.key_info(key) else {
                        continue;
                    };
                    if is_gpu_temperature_key(key, info) {
                        sensors.push(TemperatureSensor { key, info });
                    }
                }
            }
            Err(error) => warning = Some(error.message),
        }

        // Common keys remain a bounded fallback for systems that refuse key
        // enumeration while still allowing direct sensor reads.
        for key in [*b"Tg0f", *b"Tg0j", *b"TG0P", *b"TG0D"] {
            let key = fourcc(key);
            if let Ok(info) = self.key_info(key)
                && is_gpu_temperature_key(key, info)
            {
                sensors.push(TemperatureSensor { key, info });
            }
        }
        sensors.sort();
        sensors.dedup();
        sensors.truncate(MAX_GPU_TEMPERATURE_KEYS);
        (sensors, warning)
    }
}

impl Drop for SmcConnection {
    fn drop(&mut self) {
        if self.connection != 0 {
            // SAFETY: this object uniquely owns the connection returned by
            // IOServiceOpen and closes it once.
            let _ = unsafe { IOServiceClose(self.connection) };
            self.connection = 0;
        }
    }
}

struct State {
    connection: Option<SmcConnection>,
    sensors: Vec<TemperatureSensor>,
    initialization_failure: Option<SmcFailure>,
    inventory_message: Option<String>,
    matched_devices: usize,
}

impl State {
    fn initialize() -> Self {
        match SmcConnection::open() {
            Ok(connection) => {
                let (sensors, inventory_message) = connection.discover_gpu_temperature_sensors();
                Self {
                    connection: Some(connection),
                    sensors,
                    initialization_failure: None,
                    inventory_message,
                    matched_devices: 0,
                }
            }
            Err(error) => Self {
                connection: None,
                sensors: Vec::new(),
                initialization_failure: Some(error),
                inventory_message: None,
                matched_devices: 0,
            },
        }
    }
}

pub struct AppleSmcProvider {
    state: Mutex<State>,
}

impl AppleSmcProvider {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::initialize()),
        }
    }
}

impl Default for AppleSmcProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl InventoryProvider for AppleSmcProvider {
    fn provider_id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn enumerate(&self) -> Result<Vec<DeviceObservation>> {
        let mut devices = Device::all();
        #[cfg(target_arch = "aarch64")]
        devices.retain(|device| device.name().to_ascii_lowercase().contains("apple"));
        // A single SMC GPU sensor group cannot safely be assigned to multiple
        // physical GPUs. Leave it unmatched rather than duplicating readings.
        if devices.len() != 1 {
            let mut state = self.state.lock();
            state.matched_devices = 0;
            if devices.len() > 1 {
                state.inventory_message =
                    Some("AppleSMC GPU temperature is ambiguous on a multi-GPU Mac".into());
            }
            return Ok(Vec::new());
        }

        let device = devices.pop().expect("length checked above");
        let name = device.name().to_owned();
        let registry_id = format!("{:016x}", device.registry_id());
        let vendor = vendor_from_name(&name);
        let mut observation =
            DeviceObservation::new(PROVIDER_ID, registry_id.clone(), vendor, name);
        observation.identity_priority = 10;
        observation.macos = Some(MacosIdentity {
            registry_entry_id: None,
            metal_registry_id: Some(registry_id),
        });
        let mut state = self.state.lock();
        if state.connection.is_some() && !state.sensors.is_empty() {
            observation
                .capabilities
                .insert(MetricKey::TemperatureCoreCelsius);
        } else if state.connection.is_some() && state.inventory_message.is_none() {
            state.inventory_message =
                Some("AppleSMC exposed no supported Tg*/TG* temperature keys".into());
        }
        state.matched_devices = 1;
        Ok(vec![observation])
    }

    fn diagnostic(&self) -> ProviderDiagnostic {
        let state = self.state.lock();
        ProviderDiagnostic {
            id: PROVIDER_ID.into(),
            loaded: state.connection.is_some(),
            version: None,
            devices_matched: state.matched_devices,
            reason: state
                .initialization_failure
                .as_ref()
                .map(|failure| failure.reason),
            message: state
                .initialization_failure
                .as_ref()
                .map(|failure| failure.message.clone())
                .or_else(|| state.inventory_message.clone()),
        }
    }
}

impl TelemetryProvider for AppleSmcProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata::new(PROVIDER_ID, 85, 80).prefer(MetricKey::TemperatureCoreCelsius, 20)
    }

    fn capabilities(&self, device: &CanonicalGpu) -> CapabilitySet {
        let state = self.state.lock();
        if device.provider_device_ids.contains_key(PROVIDER_ID)
            && state.connection.is_some()
            && !state.sensors.is_empty()
        {
            CapabilitySet::new([MetricKey::TemperatureCoreCelsius])
        } else {
            CapabilitySet::default()
        }
    }

    fn sample(&self, device: &CanonicalGpu, request: &SampleRequest) -> Result<ProviderSample> {
        if !device.provider_device_ids.contains_key(PROVIDER_ID)
            || !request.wants(MetricKey::TemperatureCoreCelsius)
        {
            return Ok(ProviderSample::default());
        }

        let state = self.state.lock();
        let Some(connection) = state.connection.as_ref() else {
            return Ok(ProviderSample::default());
        };
        let mut values = Vec::new();
        let mut failures = Vec::new();
        for sensor in &state.sensors {
            match connection.value(*sensor) {
                Ok(value) => values.push(value),
                Err(error) => failures.push(error),
            }
        }
        let Some((temperature, quality)) = aggregate_temperature(&values) else {
            let failure = failures.into_iter().next().unwrap_or_else(|| {
                SmcFailure::temporary("AppleSMC returned no GPU temperature readings")
            });
            return Ok(ProviderSample {
                unavailable: vec![UnavailableObservation {
                    device_id: device.identity.id.clone(),
                    metric: MetricKey::TemperatureCoreCelsius,
                    reason: failure.reason,
                    source: Some(PROVIDER_ID.into()),
                    message: Some(failure.message),
                }],
                ..ProviderSample::default()
            });
        };

        Ok(ProviderSample {
            metrics: vec![MetricObservation {
                device_id: device.identity.id.clone(),
                metric: MetricKey::TemperatureCoreCelsius,
                value: MetricValue::Number(temperature),
                source: PROVIDER_ID.into(),
                quality,
                sampled_at: now_millis(),
                interval_ms: None,
                definition: Some(if values.len() == 1 {
                    "AppleSMC GPU die temperature".into()
                } else {
                    format!(
                        "arithmetic mean of {} AppleSMC GPU die temperature sensors",
                        values.len()
                    )
                }),
            }],
            ..ProviderSample::default()
        })
    }

    fn shutdown(&self) {
        self.state.lock().connection.take();
    }
}

fn vendor_from_name(name: &str) -> GpuVendor {
    let name = name.to_ascii_lowercase();
    if name.contains("apple") {
        GpuVendor::Apple
    } else if name.contains("intel") {
        GpuVendor::Intel
    } else if name.contains("amd") || name.contains("radeon") {
        GpuVendor::Amd
    } else if name.contains("nvidia") || name.contains("geforce") {
        GpuVendor::Nvidia
    } else {
        GpuVendor::Unknown
    }
}

fn aggregate_temperature(values: &[f64]) -> Option<(f64, MetricQuality)> {
    if values.is_empty() || values.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let average = values.iter().sum::<f64>() / values.len() as f64;
    Some((
        average,
        if values.len() == 1 {
            MetricQuality::Direct
        } else {
            MetricQuality::Estimated
        },
    ))
}

fn decode_temperature(info: KeyInfo, bytes: &[u8; 32]) -> std::result::Result<f64, SmcFailure> {
    let value = match (info.data_type, info.data_size) {
        (TYPE_FLT, 4) => f64::from(f32::from_le_bytes(
            bytes[..4].try_into().unwrap_or_default(),
        )),
        (TYPE_SP78, 2) => {
            f64::from(i16::from_be_bytes(
                bytes[..2].try_into().unwrap_or_default(),
            )) / 256.0
        }
        _ => {
            return Err(SmcFailure::provider(format!(
                "unsupported AppleSMC temperature type {} with size {}",
                fourcc_string(info.data_type),
                info.data_size
            )));
        }
    };
    value
        .is_finite()
        .then_some(value)
        .ok_or_else(|| SmcFailure::provider("AppleSMC temperature was not finite"))
}

fn is_gpu_temperature_key(key: u32, info: KeyInfo) -> bool {
    let key = key.to_be_bytes();
    matches!(
        (key[0], key[1], info.data_type, info.data_size),
        (b'T', b'g', TYPE_FLT, 4) | (b'T', b'G', TYPE_SP78, 2)
    )
}

const fn fourcc(bytes: [u8; 4]) -> u32 {
    u32::from_be_bytes(bytes)
}

fn fourcc_string(value: u32) -> String {
    String::from_utf8_lossy(&value.to_be_bytes()).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_apple_silicon_little_endian_float_exactly() {
        let mut bytes = [0_u8; 32];
        bytes[..4].copy_from_slice(&42.5_f32.to_le_bytes());
        let value = decode_temperature(
            KeyInfo {
                data_size: 4,
                data_type: TYPE_FLT,
                attributes: 0,
            },
            &bytes,
        )
        .expect("flt temperature");
        assert_eq!(value, 42.5);
    }

    #[test]
    fn decodes_intel_big_endian_sp78_exactly() {
        let mut bytes = [0_u8; 32];
        bytes[..2].copy_from_slice(&10_880_i16.to_be_bytes());
        let value = decode_temperature(
            KeyInfo {
                data_size: 2,
                data_type: TYPE_SP78,
                attributes: 0,
            },
            &bytes,
        )
        .expect("sp78 temperature");
        assert_eq!(value, 42.5);
    }

    #[test]
    fn key_classification_is_case_and_type_specific() {
        let float = KeyInfo {
            data_size: 4,
            data_type: TYPE_FLT,
            attributes: 0,
        };
        let sp78 = KeyInfo {
            data_size: 2,
            data_type: TYPE_SP78,
            attributes: 0,
        };
        assert!(is_gpu_temperature_key(fourcc(*b"Tg0f"), float));
        assert!(is_gpu_temperature_key(fourcc(*b"TG0P"), sp78));
        assert!(!is_gpu_temperature_key(fourcc(*b"TG0P"), float));
        assert!(!is_gpu_temperature_key(fourcc(*b"TC0P"), sp78));
    }

    #[test]
    fn multiple_sensors_are_explicitly_estimated() {
        assert_eq!(
            aggregate_temperature(&[40.0]),
            Some((40.0, MetricQuality::Direct))
        );
        assert_eq!(
            aggregate_temperature(&[40.0, 50.0]),
            Some((45.0, MetricQuality::Estimated))
        );
    }

    #[test]
    fn provider_initialization_never_panics() {
        let provider = AppleSmcProvider::new();
        assert_eq!(provider.provider_id(), PROVIDER_ID);
    }
}
