//! Optional NVIDIA NVML provider.
//!
//! `nvml-wrapper` resolves the driver library and every NVML symbol at runtime.
//! Consequently, loading `let-smi-core` never creates a link-time dependency on
//! NVIDIA software and a machine without an NVIDIA driver simply gets an
//! unloaded provider diagnostic.

#[cfg(any(target_os = "linux", windows, test))]
fn split_combined_pci_id(value: u32) -> (u32, u32) {
    // NVML packs the PCI device ID into the high word and the vendor ID into
    // the low word (for example, 0x2684_10de).
    (value & 0xffff, value >> 16)
}

#[cfg(any(target_os = "linux", windows, test))]
fn canonical_pci_address(domain: u32, bus: u32, device: u32, reported_bus_id: &str) -> String {
    let function = reported_bus_id
        .trim()
        .rsplit_once('.')
        .and_then(|(_, value)| u32::from_str_radix(value, 16).ok())
        .filter(|value| *value <= 7)
        .unwrap_or(0);
    format!("{domain:04x}:{bus:02x}:{device:02x}.{function:x}")
}

#[cfg(any(target_os = "linux", windows, test))]
fn checked_percentage(value: u32) -> std::result::Result<f64, String> {
    (value <= 100)
        .then_some(f64::from(value))
        .ok_or_else(|| format!("NVML returned an out-of-range percentage ({value})"))
}

#[cfg(any(target_os = "linux", windows, test))]
fn milliwatts_to_watts(value: u32) -> f64 {
    f64::from(value) / 1_000.0
}

#[cfg(any(target_os = "linux", windows, test))]
fn millijoules_to_joules(value: u64) -> f64 {
    value as f64 / 1_000.0
}

#[cfg(any(target_os = "linux", windows, test))]
fn microseconds_to_milliseconds(value: u32) -> u64 {
    u64::from(value).div_ceil(1_000)
}

#[cfg(any(target_os = "linux", windows))]
mod implementation {
    use super::{
        canonical_pci_address, checked_percentage, microseconds_to_milliseconds,
        millijoules_to_joules, milliwatts_to_watts, split_combined_pci_id,
    };
    use crate::error::Result;
    use crate::model::{
        CanonicalGpu, CapabilitySet, DeviceObservation, GpuKind, GpuProcessSnapshot, GpuVendor,
        MemoryTopology, Metric, MetricKey, MetricObservation, MetricQuality, MetricValue,
        PciIdentity, ProviderDiagnostic, ProviderSample, SampleRequest, StaticMemoryInfo,
        UnavailableObservation, UnavailableReason, now_millis,
    };
    use crate::provider::{InventoryProvider, ProviderMetadata, TelemetryProvider};
    use nvml_wrapper::bitmasks::device::ThrottleReasons;
    use nvml_wrapper::enum_wrappers::device::{
        Clock, ComputeMode, EncoderType, PerformanceState, TemperatureSensor, TemperatureThreshold,
    };
    use nvml_wrapper::enums::device::{DeviceArchitecture, UsedGpuMemory};
    use nvml_wrapper::error::NvmlError;
    use nvml_wrapper::struct_wrappers::device::{PciInfo, ProcessInfo};
    use nvml_wrapper::{Device, Nvml};
    use parking_lot::Mutex;
    use serde::Serialize;
    use serde_json::{Map, Value, json};
    use std::collections::BTreeMap;

    const PROVIDER_ID: &str = "nvml";
    const PROCESS_NAME_BUFFER_BYTES: usize = 1_024;
    const MAX_NVML_DEVICES: u32 = 256;
    const MAX_NVML_FANS: u32 = 64;
    const MAX_NVML_PROCESSES: usize = 16_384;
    const MAX_NVML_ENCODER_SESSIONS: usize = 4_096;
    const MAX_INVENTORY_FAILURE_DETAILS: usize = 16;
    const MAX_NVML_STRING_BYTES: usize = 4_096;

    #[derive(Debug, Clone)]
    struct DeviceRecord {
        uuid: Option<String>,
        pci_bus_id: Option<String>,
        index: u32,
        capabilities: CapabilitySet,
    }

    #[derive(Debug, Clone)]
    struct Failure {
        reason: UnavailableReason,
        message: String,
    }

    #[derive(Debug)]
    struct State {
        nvml: Option<Nvml>,
        nvml_version: Option<String>,
        driver_version: Option<String>,
        initialization_failure: Option<Failure>,
        runtime_message: Option<String>,
        inventory_message: Option<String>,
        records: BTreeMap<String, DeviceRecord>,
        closed: bool,
    }

    impl State {
        fn initialize() -> Self {
            match initialize_runtime() {
                Ok((nvml, runtime_message)) => {
                    let nvml_version = nvml.sys_nvml_version().ok().and_then(bounded_string);
                    let driver_version = nvml.sys_driver_version().ok().and_then(bounded_string);
                    Self {
                        nvml: Some(nvml),
                        nvml_version,
                        driver_version,
                        initialization_failure: None,
                        runtime_message,
                        inventory_message: None,
                        records: BTreeMap::new(),
                        closed: false,
                    }
                }
                Err(error) => Self {
                    nvml: None,
                    nvml_version: None,
                    driver_version: None,
                    initialization_failure: Some(error),
                    runtime_message: None,
                    inventory_message: None,
                    records: BTreeMap::new(),
                    closed: false,
                },
            }
        }

        #[cfg(test)]
        fn unavailable(reason: UnavailableReason, message: &str) -> Self {
            Self {
                nvml: None,
                nvml_version: None,
                driver_version: None,
                initialization_failure: Some(Failure {
                    reason,
                    message: message.into(),
                }),
                runtime_message: None,
                inventory_message: None,
                records: BTreeMap::new(),
                closed: false,
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn initialize_runtime() -> std::result::Result<(Nvml, Option<String>), Failure> {
        Nvml::init()
            .map(|nvml| (nvml, None))
            .map_err(|error| Failure {
                reason: unavailable_reason(&error),
                message: format!("could not initialize NVML: {error}"),
            })
    }

    #[cfg(windows)]
    fn initialize_runtime() -> std::result::Result<(Nvml, Option<String>), Failure> {
        crate::providers::windows::nvml_loader::initialize()
            .map(|(nvml, message)| (nvml, Some(message)))
            .map_err(|error| Failure {
                reason: error.reason,
                message: error.message,
            })
    }

    /// A single dynamically-loaded NVML runtime shared by all NVIDIA devices in
    /// one monitor.
    pub struct NvmlProvider {
        state: Mutex<State>,
    }

    impl Default for NvmlProvider {
        fn default() -> Self {
            Self::new()
        }
    }

    impl NvmlProvider {
        pub fn new() -> Self {
            Self {
                state: Mutex::new(State::initialize()),
            }
        }

        fn shutdown_runtime(&self) {
            let mut state = self.state.lock();
            if state.closed {
                return;
            }
            state.closed = true;
            state.records.clear();
            if let Some(nvml) = state.nvml.take()
                && let Err(error) = nvml.shutdown()
            {
                state.inventory_message = Some(format!("NVML shutdown reported an error: {error}"));
            }
        }
    }

    impl Drop for NvmlProvider {
        fn drop(&mut self) {
            let state = self.state.get_mut();
            if let Some(nvml) = state.nvml.take() {
                let _ = nvml.shutdown();
            }
            state.closed = true;
            state.records.clear();
        }
    }

    impl InventoryProvider for NvmlProvider {
        fn provider_id(&self) -> &'static str {
            PROVIDER_ID
        }

        fn enumerate(&self) -> Result<Vec<DeviceObservation>> {
            let mut state = self.state.lock();
            if state.closed || state.nvml.is_none() {
                return Ok(Vec::new());
            }

            let driver_version = state.driver_version.clone();
            let Some(nvml) = state.nvml.as_ref() else {
                return Ok(Vec::new());
            };
            let device_count = match nvml.device_count() {
                Ok(value) => value,
                Err(error) => {
                    state.records.clear();
                    state.inventory_message =
                        Some(format!("NVML device enumeration failed: {error}"));
                    return Ok(Vec::new());
                }
            };
            if device_count > MAX_NVML_DEVICES {
                state.records.clear();
                state.inventory_message = Some(format!(
                    "NVML reported an unreasonable device count ({device_count}); the safety limit is {MAX_NVML_DEVICES}"
                ));
                return Ok(Vec::new());
            }

            let mut observations = Vec::with_capacity(device_count as usize);
            let mut records = BTreeMap::new();
            let mut skipped = Vec::new();
            let mut skipped_count = 0_usize;

            for index in 0..device_count {
                let device = match nvml.device_by_index(index) {
                    Ok(value) => value,
                    Err(error) => {
                        skipped_count = skipped_count.saturating_add(1);
                        if skipped.len() < MAX_INVENTORY_FAILURE_DETAILS {
                            skipped.push(format!("index {index}: {error}"));
                        }
                        continue;
                    }
                };
                let uuid = device.uuid().ok().and_then(bounded_string);
                let pci_info = device.pci_info().ok();
                let pci = pci_info.as_ref().map(pci_identity);
                let canonical_bus_id = pci.as_ref().and_then(|identity| identity.address.clone());
                let provider_device_id = uuid.clone().unwrap_or_else(|| {
                    canonical_bus_id.as_ref().map_or_else(
                        || format!("index:{index}"),
                        |address| format!("pci:{address}"),
                    )
                });
                let name = device
                    .name()
                    .ok()
                    .and_then(bounded_string)
                    .unwrap_or_else(|| format!("NVIDIA GPU {index}"));
                let memory = device.memory_info().ok();
                let architecture = device
                    .architecture()
                    .ok()
                    .and_then(architecture_name)
                    .map(str::to_owned);
                let firmware_version = device.vbios_version().ok().and_then(bounded_string);
                let mut capabilities = probe_capabilities(&device, memory.is_some());
                capabilities.vendor_extensions.insert("nvidia.nvml".into());

                let mut observation = DeviceObservation::new(
                    PROVIDER_ID,
                    provider_device_id.clone(),
                    GpuVendor::Nvidia,
                    name,
                );
                observation.identity_priority = 100;
                observation.enumeration_ordinal = Some(index);
                // PCI identity is a strong correlation key, but on Linux it
                // does not by itself describe integrated/discrete topology.
                // Preserve the existing Windows behavior, where DXGI/D3DKMT
                // normally contributes the stronger device-specific signal.
                observation.kind = if cfg!(windows) && pci.is_some() {
                    GpuKind::Discrete
                } else {
                    GpuKind::Unknown
                };
                observation.uuid.clone_from(&uuid);
                observation.pci = pci;
                observation.architecture = architecture;
                observation.driver_version = driver_version.clone();
                observation.firmware_version = firmware_version.clone();
                observation.memory = StaticMemoryInfo {
                    topology: memory
                        .as_ref()
                        .map_or(MemoryTopology::Unknown, |_| MemoryTopology::Dedicated),
                    dedicated_total_bytes: memory.as_ref().map(|value| value.total),
                    shared_total_bytes: None,
                    unified_total_bytes: None,
                };
                observation.capabilities = capabilities.clone();
                observation
                    .vendor_info
                    .insert("nvmlIndex".into(), json!(index));
                if let Some(version) = firmware_version {
                    observation
                        .vendor_info
                        .insert("vbiosVersion".into(), json!(version));
                }
                add_static_vendor_info(&device, &mut observation.vendor_info);

                let record = DeviceRecord {
                    uuid,
                    pci_bus_id: pci_info.map(|value| value.bus_id).and_then(bounded_string),
                    index,
                    capabilities,
                };
                records.insert(provider_device_id, record);
                observations.push(observation);
            }

            state.records = records;
            state.inventory_message = (skipped_count > 0).then(|| {
                format!(
                    "NVML skipped {} inaccessible device(s): {}",
                    skipped_count,
                    skipped.join("; ")
                )
            });
            Ok(observations)
        }

        fn diagnostic(&self) -> ProviderDiagnostic {
            let state = self.state.lock();
            let failure = state.initialization_failure.as_ref();
            let runtime_message = match (&state.runtime_message, &state.inventory_message) {
                (Some(runtime), Some(inventory)) => Some(format!("{runtime}; {inventory}")),
                (Some(runtime), None) => Some(runtime.clone()),
                (None, Some(inventory)) => Some(inventory.clone()),
                (None, None) => None,
            };
            ProviderDiagnostic {
                id: PROVIDER_ID.into(),
                loaded: state.nvml.is_some() && !state.closed,
                version: state.nvml_version.clone(),
                devices_matched: state.records.len(),
                reason: failure.map(|value| value.reason).or_else(|| {
                    state
                        .closed
                        .then_some(UnavailableReason::TemporarilyUnavailable)
                }),
                message: failure
                    .map(|value| value.message.clone())
                    .or(runtime_message)
                    .or_else(|| state.closed.then(|| "NVML provider is shut down".into())),
            }
        }
    }

    impl TelemetryProvider for NvmlProvider {
        fn metadata(&self) -> ProviderMetadata {
            let mut metadata = ProviderMetadata::new(PROVIDER_ID, 100, 95);
            for metric in [
                MetricKey::UtilizationOverall,
                MetricKey::UtilizationMemoryController,
                MetricKey::UtilizationEncoder,
                MetricKey::UtilizationDecoder,
                MetricKey::MemoryDedicatedUsedBytes,
                MetricKey::TemperatureCoreCelsius,
                MetricKey::PowerDrawWatts,
                MetricKey::PowerLimitWatts,
                MetricKey::PowerEnergyJoules,
                MetricKey::ClockGraphicsMhz,
                MetricKey::ClockComputeMhz,
                MetricKey::ClockMemoryMhz,
                MetricKey::ClockVideoMhz,
                MetricKey::FanPercent,
                MetricKey::FanRpm,
                MetricKey::Processes,
            ] {
                metadata.metric_priorities.insert(metric, 100);
            }
            metadata
        }

        fn capabilities(&self, device: &CanonicalGpu) -> CapabilitySet {
            let Some(provider_device_id) = device.provider_device_ids.get(PROVIDER_ID) else {
                return CapabilitySet::default();
            };
            self.state
                .lock()
                .records
                .get(provider_device_id)
                .map_or_else(CapabilitySet::default, |record| record.capabilities.clone())
        }

        fn sample(
            &self,
            canonical: &CanonicalGpu,
            request: &SampleRequest,
        ) -> Result<ProviderSample> {
            let Some(provider_device_id) = canonical.provider_device_ids.get(PROVIDER_ID) else {
                return Ok(ProviderSample::default());
            };
            let state = self.state.lock();
            let Some(record) = state.records.get(provider_device_id) else {
                return Ok(ProviderSample::default());
            };
            let capabilities = record.capabilities.clone();
            let mut sample = ProviderSample::default();
            let Some(nvml) = state.nvml.as_ref() else {
                unavailable_for_all(
                    &mut sample,
                    canonical,
                    request,
                    &capabilities,
                    UnavailableReason::DriverLibraryMissing,
                    "NVML is no longer available".into(),
                );
                return Ok(sample);
            };
            let device = match resolve_device(nvml, record) {
                Ok(value) => value,
                Err(error) => {
                    unavailable_for_all(
                        &mut sample,
                        canonical,
                        request,
                        &capabilities,
                        unavailable_reason(&error),
                        format!("NVML could not reacquire the device: {error}"),
                    );
                    return Ok(sample);
                }
            };

            sample_metrics(
                nvml,
                &device,
                canonical,
                request,
                &capabilities,
                &mut sample,
            );
            Ok(sample)
        }

        fn vendor_info(&self, canonical: &CanonicalGpu) -> Result<Value> {
            let Some(provider_device_id) = canonical.provider_device_ids.get(PROVIDER_ID) else {
                return Ok(Value::Null);
            };
            let state = self.state.lock();
            let (Some(nvml), Some(record)) =
                (state.nvml.as_ref(), state.records.get(provider_device_id))
            else {
                return Ok(Value::Null);
            };
            let Ok(device) = resolve_device(nvml, record) else {
                return Ok(Value::Null);
            };
            Ok(Value::Object(dynamic_vendor_info(&device)))
        }

        fn shutdown(&self) {
            self.shutdown_runtime();
        }
    }

    fn pci_identity(info: &PciInfo) -> PciIdentity {
        let (vendor_id, device_id) = split_combined_pci_id(info.pci_device_id);
        let (subsystem_vendor_id, subsystem_device_id) = info
            .pci_sub_system_id
            .map(split_combined_pci_id)
            .map_or((None, None), |(vendor, device)| {
                (Some(vendor), Some(device))
            });
        PciIdentity {
            address: Some(canonical_pci_address(
                info.domain,
                info.bus,
                info.device,
                &info.bus_id,
            )),
            vendor_id,
            device_id,
            subsystem_vendor_id,
            subsystem_device_id,
        }
    }

    fn bounded_string(value: String) -> Option<String> {
        (value.len() <= MAX_NVML_STRING_BYTES).then_some(value)
    }

    fn architecture_name(value: DeviceArchitecture) -> Option<&'static str> {
        match value {
            DeviceArchitecture::Kepler => Some("Kepler"),
            DeviceArchitecture::Maxwell => Some("Maxwell"),
            DeviceArchitecture::Pascal => Some("Pascal"),
            DeviceArchitecture::Volta => Some("Volta"),
            DeviceArchitecture::Turing => Some("Turing"),
            DeviceArchitecture::Ampere => Some("Ampere"),
            DeviceArchitecture::Ada => Some("Ada Lovelace"),
            DeviceArchitecture::Hopper => Some("Hopper"),
            DeviceArchitecture::Blackwell => Some("Blackwell"),
            DeviceArchitecture::Unknown => None,
        }
    }

    fn probe_capabilities(device: &Device<'_>, memory_supported: bool) -> CapabilitySet {
        let mut capabilities = CapabilitySet::default();
        if device.utilization_rates().is_ok() {
            capabilities.insert(MetricKey::UtilizationOverall);
            capabilities.insert(MetricKey::UtilizationMemoryController);
        }
        if device.encoder_utilization().is_ok() {
            capabilities.insert(MetricKey::UtilizationEncoder);
        }
        if device.decoder_utilization().is_ok() {
            capabilities.insert(MetricKey::UtilizationDecoder);
        }
        if memory_supported {
            capabilities.insert(MetricKey::MemoryDedicatedUsedBytes);
        }
        if device.temperature(TemperatureSensor::Gpu).is_ok() {
            capabilities.insert(MetricKey::TemperatureCoreCelsius);
        }
        if device.power_usage().is_ok() {
            capabilities.insert(MetricKey::PowerDrawWatts);
        }
        if device.enforced_power_limit().is_ok() {
            capabilities.insert(MetricKey::PowerLimitWatts);
        }
        if device.total_energy_consumption().is_ok() {
            capabilities.insert(MetricKey::PowerEnergyJoules);
        }
        for (clock, metric) in [
            (Clock::Graphics, MetricKey::ClockGraphicsMhz),
            (Clock::SM, MetricKey::ClockComputeMhz),
            (Clock::Memory, MetricKey::ClockMemoryMhz),
            (Clock::Video, MetricKey::ClockVideoMhz),
        ] {
            if device.clock_info(clock).is_ok() {
                capabilities.insert(metric);
            }
        }
        if average_fan_percentage(device).is_ok() {
            capabilities.insert(MetricKey::FanPercent);
        }
        if average_fan_reading(device, |device, index| device.fan_speed_rpm(index)).is_ok() {
            capabilities.insert(MetricKey::FanRpm);
        }
        if device.running_compute_processes_count().is_ok()
            || device.running_graphics_processes_count().is_ok()
        {
            capabilities.insert(MetricKey::Processes);
        }
        capabilities
    }

    fn resolve_device<'nvml>(
        nvml: &'nvml Nvml,
        record: &DeviceRecord,
    ) -> std::result::Result<Device<'nvml>, NvmlError> {
        if let Some(uuid) = &record.uuid {
            return nvml.device_by_uuid(uuid.as_str());
        }
        if let Some(bus_id) = &record.pci_bus_id {
            return nvml.device_by_pci_bus_id(bus_id.as_str());
        }
        nvml.device_by_index(record.index)
    }

    fn sample_metrics(
        nvml: &Nvml,
        device: &Device<'_>,
        canonical: &CanonicalGpu,
        request: &SampleRequest,
        capabilities: &CapabilitySet,
        sample: &mut ProviderSample,
    ) {
        let sampled_at = now_millis();
        let wants = |metric| capabilities.supports(metric) && request.wants(metric);

        let wants_overall = wants(MetricKey::UtilizationOverall);
        let wants_memory_controller = wants(MetricKey::UtilizationMemoryController);
        if wants_overall || wants_memory_controller {
            match device.utilization_rates() {
                Ok(utilization) => {
                    if wants_overall {
                        push_percentage_value(
                            sample,
                            canonical,
                            MetricKey::UtilizationOverall,
                            utilization.gpu,
                            sampled_at,
                            None,
                            Some(
                                "percentage of NVML's internal sample period during which one or more kernels executed on the GPU",
                            ),
                        );
                    }
                    if wants_memory_controller {
                        push_percentage_value(
                            sample,
                            canonical,
                            MetricKey::UtilizationMemoryController,
                            utilization.memory,
                            sampled_at,
                            None,
                            Some(
                                "percentage of NVML's internal sample period during which device memory was read or written",
                            ),
                        );
                    }
                }
                Err(error) => {
                    for metric in [
                        MetricKey::UtilizationOverall,
                        MetricKey::UtilizationMemoryController,
                    ] {
                        if wants(metric) {
                            push_nvml_error(sample, canonical, metric, "utilization", &error);
                        }
                    }
                }
            }
        }

        if wants(MetricKey::UtilizationEncoder) {
            match device.encoder_utilization() {
                Ok(value) => push_percentage_value(
                    sample,
                    canonical,
                    MetricKey::UtilizationEncoder,
                    value.utilization,
                    sampled_at,
                    Some(microseconds_to_milliseconds(value.sampling_period)),
                    Some("NVENC busy percentage over NVML's reported sampling period"),
                ),
                Err(error) => push_nvml_error(
                    sample,
                    canonical,
                    MetricKey::UtilizationEncoder,
                    "encoder utilization",
                    &error,
                ),
            }
        }
        if wants(MetricKey::UtilizationDecoder) {
            match device.decoder_utilization() {
                Ok(value) => push_percentage_value(
                    sample,
                    canonical,
                    MetricKey::UtilizationDecoder,
                    value.utilization,
                    sampled_at,
                    Some(microseconds_to_milliseconds(value.sampling_period)),
                    Some("NVDEC busy percentage over NVML's reported sampling period"),
                ),
                Err(error) => push_nvml_error(
                    sample,
                    canonical,
                    MetricKey::UtilizationDecoder,
                    "decoder utilization",
                    &error,
                ),
            }
        }

        if wants(MetricKey::MemoryDedicatedUsedBytes) {
            push_numeric_result(
                sample,
                canonical,
                MetricKey::MemoryDedicatedUsedBytes,
                "framebuffer memory",
                device
                    .memory_info()
                    .map(|value| value.used as f64)
                    .map_err(FieldError::Nvml),
                sampled_at,
                None,
                None,
            );
        }
        if wants(MetricKey::TemperatureCoreCelsius) {
            push_numeric_result(
                sample,
                canonical,
                MetricKey::TemperatureCoreCelsius,
                "GPU temperature",
                device
                    .temperature(TemperatureSensor::Gpu)
                    .map(f64::from)
                    .map_err(FieldError::Nvml),
                sampled_at,
                None,
                None,
            );
        }
        if wants(MetricKey::PowerDrawWatts) {
            push_numeric_result(
                sample,
                canonical,
                MetricKey::PowerDrawWatts,
                "power draw",
                device
                    .power_usage()
                    .map(milliwatts_to_watts)
                    .map_err(FieldError::Nvml),
                sampled_at,
                None,
                None,
            );
        }
        if wants(MetricKey::PowerLimitWatts) {
            push_numeric_result(
                sample,
                canonical,
                MetricKey::PowerLimitWatts,
                "enforced power limit",
                device
                    .enforced_power_limit()
                    .map(milliwatts_to_watts)
                    .map_err(FieldError::Nvml),
                sampled_at,
                None,
                Some("effective power limit currently enforced by the NVIDIA driver"),
            );
        }
        if wants(MetricKey::PowerEnergyJoules) {
            push_numeric_result(
                sample,
                canonical,
                MetricKey::PowerEnergyJoules,
                "total energy",
                device
                    .total_energy_consumption()
                    .map(millijoules_to_joules)
                    .map_err(FieldError::Nvml),
                sampled_at,
                None,
                Some("cumulative energy since the last NVIDIA driver reload"),
            );
        }

        for (clock, metric, label) in [
            (
                Clock::Graphics,
                MetricKey::ClockGraphicsMhz,
                "graphics clock",
            ),
            (Clock::SM, MetricKey::ClockComputeMhz, "SM clock"),
            (Clock::Memory, MetricKey::ClockMemoryMhz, "memory clock"),
            (Clock::Video, MetricKey::ClockVideoMhz, "video clock"),
        ] {
            if wants(metric) {
                push_numeric_result(
                    sample,
                    canonical,
                    metric,
                    label,
                    device
                        .clock_info(clock)
                        .map(f64::from)
                        .map_err(FieldError::Nvml),
                    sampled_at,
                    None,
                    None,
                );
            }
        }

        if wants(MetricKey::FanPercent) {
            push_numeric_result(
                sample,
                canonical,
                MetricKey::FanPercent,
                "fan speed percentage",
                average_fan_percentage(device),
                sampled_at,
                None,
                Some("arithmetic mean of all NVML-reported GPU fan percentages"),
            );
        }
        if wants(MetricKey::FanRpm) {
            push_numeric_result(
                sample,
                canonical,
                MetricKey::FanRpm,
                "fan speed RPM",
                average_fan_reading(device, |device, index| device.fan_speed_rpm(index)),
                sampled_at,
                None,
                Some("arithmetic mean of all NVML-reported GPU fan RPM values"),
            );
        }

        if request.include_processes && capabilities.supports(MetricKey::Processes) {
            match collect_processes(nvml, device, sampled_at) {
                Ok(processes) => sample.processes = Some(processes),
                Err(FieldError::Nvml(error)) => push_nvml_error(
                    sample,
                    canonical,
                    MetricKey::Processes,
                    "process accounting",
                    &error,
                ),
                Err(FieldError::InvalidValue(message)) => {
                    push_invalid_value(sample, canonical, MetricKey::Processes, message)
                }
            }
        }
    }

    #[derive(Debug)]
    enum FieldError {
        Nvml(NvmlError),
        InvalidValue(String),
    }

    #[allow(clippy::too_many_arguments)]
    fn push_numeric_result(
        sample: &mut ProviderSample,
        canonical: &CanonicalGpu,
        metric: MetricKey,
        operation: &str,
        result: std::result::Result<f64, FieldError>,
        sampled_at: u64,
        interval_ms: Option<u64>,
        definition: Option<&str>,
    ) {
        match result {
            Ok(value) if value.is_finite() && value >= 0.0 => {
                sample.metrics.push(MetricObservation {
                    device_id: canonical.identity.id.clone(),
                    metric,
                    value: MetricValue::Number(value),
                    source: PROVIDER_ID.into(),
                    quality: MetricQuality::Direct,
                    sampled_at,
                    interval_ms,
                    definition: definition.map(str::to_owned),
                });
            }
            Ok(value) => push_invalid_value(
                sample,
                canonical,
                metric,
                format!("NVML {operation} returned invalid value {value}"),
            ),
            Err(FieldError::Nvml(error)) => {
                push_nvml_error(sample, canonical, metric, operation, &error)
            }
            Err(FieldError::InvalidValue(message)) => {
                push_invalid_value(sample, canonical, metric, message)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_percentage_value(
        sample: &mut ProviderSample,
        canonical: &CanonicalGpu,
        metric: MetricKey,
        value: u32,
        sampled_at: u64,
        interval_ms: Option<u64>,
        definition: Option<&str>,
    ) {
        push_numeric_result(
            sample,
            canonical,
            metric,
            "percentage",
            checked_percentage(value).map_err(FieldError::InvalidValue),
            sampled_at,
            interval_ms,
            definition,
        );
    }

    fn push_invalid_value(
        sample: &mut ProviderSample,
        canonical: &CanonicalGpu,
        metric: MetricKey,
        message: String,
    ) {
        sample.unavailable.push(UnavailableObservation {
            device_id: canonical.identity.id.clone(),
            metric,
            reason: UnavailableReason::ProviderError,
            source: Some(PROVIDER_ID.into()),
            message: Some(message),
        });
    }

    fn push_nvml_error(
        sample: &mut ProviderSample,
        canonical: &CanonicalGpu,
        metric: MetricKey,
        operation: &str,
        error: &NvmlError,
    ) {
        sample.unavailable.push(UnavailableObservation {
            device_id: canonical.identity.id.clone(),
            metric,
            reason: unavailable_reason(error),
            source: Some(PROVIDER_ID.into()),
            message: Some(format!("NVML {operation} query failed: {error}")),
        });
    }

    fn unavailable_for_all(
        sample: &mut ProviderSample,
        canonical: &CanonicalGpu,
        request: &SampleRequest,
        capabilities: &CapabilitySet,
        reason: UnavailableReason,
        message: String,
    ) {
        sample.unavailable.extend(
            capabilities
                .metrics()
                .filter(|metric| {
                    (*metric != MetricKey::Processes && request.wants(*metric))
                        || (*metric == MetricKey::Processes && request.include_processes)
                })
                .map(|metric| UnavailableObservation {
                    device_id: canonical.identity.id.clone(),
                    metric,
                    reason,
                    source: Some(PROVIDER_ID.into()),
                    message: Some(message.clone()),
                }),
        );
    }

    fn average_fan_reading(
        device: &Device<'_>,
        mut read: impl FnMut(&Device<'_>, u32) -> std::result::Result<u32, NvmlError>,
    ) -> std::result::Result<f64, FieldError> {
        let (fan_count, mut last_error) = match device.num_fans() {
            Ok(0) => return Err(FieldError::Nvml(NvmlError::NotSupported)),
            Ok(value) => (value, None),
            Err(error) => (1, Some(error)),
        };
        if fan_count > MAX_NVML_FANS {
            return Err(FieldError::InvalidValue(format!(
                "NVML reported an unreasonable fan count ({fan_count}); the safety limit is {MAX_NVML_FANS}"
            )));
        }
        let mut values = Vec::with_capacity(fan_count as usize);
        for index in 0..fan_count {
            match read(device, index) {
                Ok(value) => values.push(value),
                Err(error) => last_error = Some(error),
            }
        }
        if values.is_empty() {
            return Err(FieldError::Nvml(
                last_error.unwrap_or(NvmlError::NotSupported),
            ));
        }
        let total: u64 = values.iter().map(|value| u64::from(*value)).sum();
        let average = total as f64 / values.len() as f64;
        if average.is_finite() {
            Ok(average)
        } else {
            Err(FieldError::InvalidValue(
                "NVML returned invalid fan readings".into(),
            ))
        }
    }

    fn average_fan_percentage(device: &Device<'_>) -> std::result::Result<f64, FieldError> {
        let value = average_fan_reading(device, |device, index| device.fan_speed(index))?;
        if (0.0..=100.0).contains(&value) {
            Ok(value)
        } else {
            Err(FieldError::InvalidValue(format!(
                "NVML returned an out-of-range fan percentage ({value})"
            )))
        }
    }

    fn collect_processes(
        nvml: &Nvml,
        device: &Device<'_>,
        sampled_at: u64,
    ) -> std::result::Result<Vec<GpuProcessSnapshot>, FieldError> {
        let mut processes: BTreeMap<u32, Option<u64>> = BTreeMap::new();
        let mut successful_queries = 0_u8;
        let mut last_error = None;
        for graphics in [false, true] {
            let count = if graphics {
                device.running_graphics_processes_count()
            } else {
                device.running_compute_processes_count()
            };
            let count = match count {
                Ok(value) => usize::try_from(value).map_err(|_| {
                    FieldError::InvalidValue("NVML process count does not fit usize".into())
                })?,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            if count > MAX_NVML_PROCESSES {
                return Err(FieldError::InvalidValue(format!(
                    "NVML reported an unreasonable process count ({count}); the safety limit is {MAX_NVML_PROCESSES}"
                )));
            }
            let result = if graphics {
                device.running_graphics_processes()
            } else {
                device.running_compute_processes()
            };
            match result {
                Ok(values) => {
                    successful_queries += 1;
                    merge_process_readings(&mut processes, values);
                }
                Err(error) => last_error = Some(error),
            }
        }
        if successful_queries == 0 {
            return Err(FieldError::Nvml(
                last_error.unwrap_or(NvmlError::NotSupported),
            ));
        }

        Ok(processes
            .into_iter()
            .map(|(pid, used_memory)| GpuProcessSnapshot {
                pid,
                name: nvml.sys_process_name(pid, PROCESS_NAME_BUFFER_BYTES).ok(),
                memory_used_bytes: used_memory.map(|bytes| {
                    Metric::available(
                        bytes as f64,
                        PROVIDER_ID.into(),
                        MetricQuality::Direct,
                        sampled_at,
                        None,
                        Some("NVML-reported framebuffer allocation for this process".into()),
                    )
                }),
                utilization: None,
            })
            .collect())
    }

    fn merge_process_readings(
        destination: &mut BTreeMap<u32, Option<u64>>,
        readings: Vec<ProcessInfo>,
    ) {
        for process in readings {
            let memory = match process.used_gpu_memory {
                UsedGpuMemory::Used(value) => Some(value),
                UsedGpuMemory::Unavailable => None,
            };
            if !destination.contains_key(&process.pid) && destination.len() >= MAX_NVML_PROCESSES {
                continue;
            }
            destination
                .entry(process.pid)
                .and_modify(|current| {
                    *current = match (*current, memory) {
                        (Some(left), Some(right)) => Some(left.max(right)),
                        (Some(value), None) | (None, Some(value)) => Some(value),
                        (None, None) => None,
                    };
                })
                .or_insert(memory);
        }
    }

    fn unavailable_reason(error: &NvmlError) -> UnavailableReason {
        match error {
            NvmlError::NotSupported
            | NvmlError::FunctionNotFound
            | NvmlError::FailedToLoadSymbol(_) => UnavailableReason::Unsupported,
            NvmlError::NoPermission | NvmlError::OperatingSystem => {
                UnavailableReason::PermissionDenied
            }
            NvmlError::GpuLost | NvmlError::ResetRequired | NvmlError::NotFound => {
                UnavailableReason::DeviceLost
            }
            NvmlError::DriverNotLoaded
            | NvmlError::LibraryNotFound
            | NvmlError::LibloadingError(_)
            | NvmlError::LibRmVersionMismatch => UnavailableReason::DriverLibraryMissing,
            NvmlError::Timeout
            | NvmlError::NoData
            | NvmlError::InUse
            | NvmlError::InsufficientPower
            | NvmlError::IrqIssue => UnavailableReason::TemporarilyUnavailable,
            _ => UnavailableReason::ProviderError,
        }
    }

    fn add_static_vendor_info(device: &Device<'_>, destination: &mut BTreeMap<String, Value>) {
        if let Ok(value) = device.cuda_compute_capability() {
            destination.insert(
                "cudaComputeCapability".into(),
                json!({ "major": value.major, "minor": value.minor }),
            );
        }
        if let Ok(value) = device.attributes() {
            destination.insert("smCount".into(), json!(value.multiprocessor_count));
        }
        match device.is_ecc_enabled() {
            Ok(value) => {
                destination.insert(
                    "ecc".into(),
                    json!({
                        "supported": true,
                        "enabled": value.currently_enabled,
                    }),
                );
            }
            Err(error) if is_unsupported(&error) => {
                destination.insert("ecc".into(), json!({ "supported": false }));
            }
            Err(_) => {}
        }
        match device.mig_mode() {
            Ok(value) => {
                destination.insert(
                    "mig".into(),
                    json!({ "supported": true, "enabled": value.current != 0 }),
                );
            }
            Err(error) if is_unsupported(&error) => {
                destination.insert("mig".into(), json!({ "supported": false }));
            }
            Err(_) => {}
        }
        if let Ok(value) = device.bar1_memory_info() {
            destination.insert("bar1TotalBytes".into(), json!(value.total));
        }
        if let Ok(value) = device.compute_mode() {
            destination.insert("computeMode".into(), json!(compute_mode_name(value)));
        }
    }

    fn dynamic_vendor_info(device: &Device<'_>) -> Map<String, Value> {
        let sampled_at = now_millis();
        let mut info = Map::new();
        if let Ok(value) = device.performance_state()
            && let Some(value) = performance_state_number(value)
        {
            info.insert("pState".into(), metric_json(value, sampled_at));
        }
        if let Ok(value) = device.current_throttle_reasons() {
            info.insert(
                "throttleReasons".into(),
                metric_json(throttle_reason_names(value), sampled_at),
            );
        }
        if let Ok(value) = device.bar1_memory_info() {
            info.insert("bar1TotalBytes".into(), json!(value.total));
            info.insert(
                "bar1UsedBytes".into(),
                metric_json(value.used as f64, sampled_at),
            );
        }
        if let Ok(value) = device.current_pcie_link_gen() {
            info.insert("pcieGeneration".into(), metric_json(value, sampled_at));
        }
        if let Ok(value) = device.current_pcie_link_width() {
            info.insert("pcieWidth".into(), metric_json(value, sampled_at));
        }
        if let Ok(mut values) = device.encoder_sessions() {
            let truncated = values.len() > MAX_NVML_ENCODER_SESSIONS;
            values.truncate(MAX_NVML_ENCODER_SESSIONS);
            info.insert(
                "encoderSessions".into(),
                Value::Array(
                    values
                        .into_iter()
                        .map(|session| {
                            json!({
                                "pid": session.pid,
                                "codec": encoder_name(session.codec_type),
                                "width": session.hres,
                                "height": session.vres,
                                "averageFps": session.average_fps,
                            })
                        })
                        .collect(),
                ),
            );
            if truncated {
                info.insert("encoderSessionsTruncated".into(), Value::Bool(true));
            }
        }
        let mut thresholds = Map::new();
        for (threshold, name) in [
            (TemperatureThreshold::Shutdown, "shutdown"),
            (TemperatureThreshold::Slowdown, "slowdown"),
            (TemperatureThreshold::MemoryMax, "memoryMax"),
            (TemperatureThreshold::GpuMax, "gpuMax"),
        ] {
            if let Ok(value) = device.temperature_threshold(threshold) {
                thresholds.insert(name.into(), json!(value));
            }
        }
        if !thresholds.is_empty() {
            info.insert("thermalThresholdsCelsius".into(), Value::Object(thresholds));
        }
        info
    }

    fn metric_json<T: Serialize>(value: T, sampled_at: u64) -> Value {
        serde_json::to_value(Metric::available(
            value,
            PROVIDER_ID.into(),
            MetricQuality::Direct,
            sampled_at,
            None,
            None,
        ))
        .unwrap_or(Value::Null)
    }

    fn performance_state_number(value: PerformanceState) -> Option<u32> {
        Some(match value {
            PerformanceState::Zero => 0,
            PerformanceState::One => 1,
            PerformanceState::Two => 2,
            PerformanceState::Three => 3,
            PerformanceState::Four => 4,
            PerformanceState::Five => 5,
            PerformanceState::Six => 6,
            PerformanceState::Seven => 7,
            PerformanceState::Eight => 8,
            PerformanceState::Nine => 9,
            PerformanceState::Ten => 10,
            PerformanceState::Eleven => 11,
            PerformanceState::Twelve => 12,
            PerformanceState::Thirteen => 13,
            PerformanceState::Fourteen => 14,
            PerformanceState::Fifteen => 15,
            PerformanceState::Unknown => return None,
        })
    }

    fn is_unsupported(error: &NvmlError) -> bool {
        matches!(
            error,
            NvmlError::NotSupported
                | NvmlError::FunctionNotFound
                | NvmlError::FailedToLoadSymbol(_)
        )
    }

    fn compute_mode_name(value: ComputeMode) -> &'static str {
        match value {
            ComputeMode::Default => "default",
            ComputeMode::ExclusiveThread => "exclusive-thread",
            ComputeMode::Prohibited => "prohibited",
            ComputeMode::ExclusiveProcess => "exclusive-process",
        }
    }

    fn encoder_name(value: EncoderType) -> &'static str {
        match value {
            EncoderType::H264 => "h264",
            EncoderType::HEVC => "hevc",
        }
    }

    fn throttle_reason_names(value: ThrottleReasons) -> Vec<&'static str> {
        let candidates = [
            (ThrottleReasons::GPU_IDLE, "gpu-idle"),
            (
                ThrottleReasons::APPLICATIONS_CLOCKS_SETTING,
                "applications-clocks-setting",
            ),
            (ThrottleReasons::SW_POWER_CAP, "software-power-cap"),
            (ThrottleReasons::HW_SLOWDOWN, "hardware-slowdown"),
            (ThrottleReasons::SYNC_BOOST, "sync-boost"),
            (
                ThrottleReasons::SW_THERMAL_SLOWDOWN,
                "software-thermal-slowdown",
            ),
            (
                ThrottleReasons::HW_THERMAL_SLOWDOWN,
                "hardware-thermal-slowdown",
            ),
            (
                ThrottleReasons::HW_POWER_BRAKE_SLOWDOWN,
                "hardware-power-brake-slowdown",
            ),
            (
                ThrottleReasons::DISPLAY_CLOCK_SETTING,
                "display-clock-setting",
            ),
        ];
        candidates
            .into_iter()
            .filter_map(|(flag, name)| value.contains(flag).then_some(name))
            .collect()
    }

    #[cfg(test)]
    mod platform_tests {
        use super::*;

        #[test]
        fn maps_nvml_errors_to_public_availability_reasons() {
            assert_eq!(
                unavailable_reason(&NvmlError::NotSupported),
                UnavailableReason::Unsupported
            );
            assert_eq!(
                unavailable_reason(&NvmlError::NoPermission),
                UnavailableReason::PermissionDenied
            );
            assert_eq!(
                unavailable_reason(&NvmlError::GpuLost),
                UnavailableReason::DeviceLost
            );
            assert_eq!(
                unavailable_reason(&NvmlError::DriverNotLoaded),
                UnavailableReason::DriverLibraryMissing
            );
            assert_eq!(
                unavailable_reason(&NvmlError::NoData),
                UnavailableReason::TemporarilyUnavailable
            );
        }

        #[test]
        fn bounds_strings_returned_by_the_driver() {
            assert_eq!(
                bounded_string("driver-value".into()).as_deref(),
                Some("driver-value")
            );
            assert!(bounded_string("x".repeat(MAX_NVML_STRING_BYTES + 1)).is_none());
        }

        #[test]
        fn a_missing_nvml_runtime_degrades_without_inventory_failure() {
            let provider = NvmlProvider {
                state: Mutex::new(State::unavailable(
                    UnavailableReason::DriverLibraryMissing,
                    "mock NVML library is absent",
                )),
            };

            assert!(provider.enumerate().expect("optional inventory").is_empty());
            let diagnostic = provider.diagnostic();
            assert!(!diagnostic.loaded);
            assert_eq!(
                diagnostic.reason,
                Some(UnavailableReason::DriverLibraryMissing)
            );
        }
    }
}

#[cfg(any(target_os = "linux", windows))]
pub use implementation::NvmlProvider;

#[cfg(not(any(target_os = "linux", windows)))]
mod unsupported {
    use crate::error::Result;
    use crate::model::{
        CanonicalGpu, CapabilitySet, DeviceObservation, ProviderDiagnostic, ProviderSample,
        SampleRequest, UnavailableReason,
    };
    use crate::provider::{InventoryProvider, ProviderMetadata, TelemetryProvider};

    #[derive(Default)]
    pub struct NvmlProvider;

    impl NvmlProvider {
        pub fn new() -> Self {
            Self
        }
    }

    impl InventoryProvider for NvmlProvider {
        fn provider_id(&self) -> &'static str {
            "nvml"
        }

        fn enumerate(&self) -> Result<Vec<DeviceObservation>> {
            Ok(Vec::new())
        }

        fn diagnostic(&self) -> ProviderDiagnostic {
            ProviderDiagnostic {
                id: self.provider_id().into(),
                loaded: false,
                version: None,
                devices_matched: 0,
                reason: Some(UnavailableReason::Unsupported),
                message: Some("NVML is only supported on Windows and Linux".into()),
            }
        }
    }

    impl TelemetryProvider for NvmlProvider {
        fn metadata(&self) -> ProviderMetadata {
            ProviderMetadata::new(self.provider_id(), 100, 95)
        }

        fn capabilities(&self, _device: &CanonicalGpu) -> CapabilitySet {
            CapabilitySet::default()
        }

        fn sample(
            &self,
            _device: &CanonicalGpu,
            _request: &SampleRequest,
        ) -> Result<ProviderSample> {
            Ok(ProviderSample::default())
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub use unsupported::NvmlProvider;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_nvml_combined_pci_ids() {
        assert_eq!(split_combined_pci_id(0x2684_10de), (0x10de, 0x2684));
        assert_eq!(split_combined_pci_id(0x163a_1043), (0x1043, 0x163a));
    }

    #[test]
    fn canonicalizes_nvmls_eight_digit_pci_domain() {
        assert_eq!(
            canonical_pci_address(0, 0x65, 0, "00000000:65:00.3"),
            "0000:65:00.3"
        );
        assert_eq!(
            canonical_pci_address(1, 2, 3, "00000001:02:03.0"),
            "0001:02:03.0"
        );
    }

    #[test]
    fn distinguishes_zero_from_invalid_percentages() {
        assert_eq!(checked_percentage(0), Ok(0.0));
        assert_eq!(checked_percentage(100), Ok(100.0));
        assert!(checked_percentage(101).is_err());
    }

    #[test]
    fn converts_nvml_units_to_public_units() {
        assert_eq!(milliwatts_to_watts(125_500), 125.5);
        assert_eq!(millijoules_to_joules(2_500), 2.5);
        assert_eq!(microseconds_to_milliseconds(1), 1);
        assert_eq!(microseconds_to_milliseconds(1_001), 2);
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    #[test]
    fn unsupported_platform_stub_degrades_without_error() {
        use crate::provider::InventoryProvider;

        let provider = NvmlProvider::new();
        assert!(provider.enumerate().unwrap().is_empty());
        let diagnostic = provider.diagnostic();
        assert!(!diagnostic.loaded);
        assert_eq!(
            diagnostic.reason,
            Some(crate::model::UnavailableReason::Unsupported)
        );
    }
}
