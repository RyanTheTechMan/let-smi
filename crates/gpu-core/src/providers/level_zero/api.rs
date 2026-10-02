use super::ffi::*;
use crate::error::{GpuError, Result};
use crate::model::UnavailableReason;
use libloading::Library;

macro_rules! api {
    ($($field:ident: $symbol:literal ($($arg:ty),*)),* $(,)?) => {
        pub(super) struct Api {
            // The library outlives every function pointer and device handle.
            _library: Option<Library>,
            $(pub(super) $field: unsafe extern "C" fn($($arg),*) -> ze_result_t,)*
        }
        impl Api {
            pub(super) fn load() -> Result<Self> {
                let library = load_library()?;
                // SAFETY: signatures are from the pinned C headers. Required
                // symbols are checked before any call and the library is retained.
                unsafe {
                    Ok(Self { $($field: *library.get(concat!($symbol, "\0").as_bytes()).map_err(|error| failure(UnavailableReason::Unsupported, format!("missing {}: {error}", $symbol)))?,)* _library: Some(library) })
                }
            }
            #[cfg(test)]
            #[allow(clippy::too_many_arguments)]
            pub(super) fn fixture($($field: unsafe extern "C" fn($($arg),*) -> ze_result_t),*) -> Self {
                Self { _library: None, $($field,)* }
            }
        }
    };
}

api! {
    init: "zesInit" (u32),
    drivers: "zesDriverGet" (*mut u32, *mut zes_driver_handle_t),
    devices: "zesDeviceGet" (zes_driver_handle_t, *mut u32, *mut zes_device_handle_t),
    device_properties: "zesDeviceGetProperties" (zes_device_handle_t, *mut zes_device_properties_t),
    pci: "zesDevicePciGetProperties" (zes_device_handle_t, *mut zes_pci_properties_t),
    engines: "zesDeviceEnumEngineGroups" (zes_device_handle_t, *mut u32, *mut zes_engine_handle_t),
    engine_properties: "zesEngineGetProperties" (zes_engine_handle_t, *mut zes_engine_properties_t),
    activity: "zesEngineGetActivity" (zes_engine_handle_t, *mut zes_engine_stats_t),
    frequencies: "zesDeviceEnumFrequencyDomains" (zes_device_handle_t, *mut u32, *mut zes_freq_handle_t),
    frequency_properties: "zesFrequencyGetProperties" (zes_freq_handle_t, *mut zes_freq_properties_t),
    frequency: "zesFrequencyGetState" (zes_freq_handle_t, *mut zes_freq_state_t),
    temperatures: "zesDeviceEnumTemperatureSensors" (zes_device_handle_t, *mut u32, *mut zes_temp_handle_t),
    temperature_properties: "zesTemperatureGetProperties" (zes_temp_handle_t, *mut zes_temp_properties_t),
    temperature: "zesTemperatureGetState" (zes_temp_handle_t, *mut f64),
    powers: "zesDeviceEnumPowerDomains" (zes_device_handle_t, *mut u32, *mut zes_pwr_handle_t),
    power_properties: "zesPowerGetProperties" (zes_pwr_handle_t, *mut zes_power_properties_t),
    energy: "zesPowerGetEnergyCounter" (zes_pwr_handle_t, *mut zes_power_energy_counter_t),
}

#[cfg(windows)]
fn load_library() -> Result<Library> {
    use libloading::os::windows::Library as WindowsLibrary;
    use windows::Win32::System::LibraryLoader::LOAD_LIBRARY_SEARCH_SYSTEM32;
    // SAFETY: the system-only flag excludes the working directory and PATH.
    unsafe { WindowsLibrary::load_with_flags("ze_loader.dll", LOAD_LIBRARY_SEARCH_SYSTEM32.0) }
        .map(Into::into)
        .map_err(|error| failure(UnavailableReason::DriverLibraryMissing, error.to_string()))
}

#[cfg(target_os = "linux")]
fn load_library() -> Result<Library> {
    // SAFETY: optional system runtime loaded with the same policy as Linux NVML.
    unsafe { Library::new("libze_loader.so.1") }
        .map_err(|error| failure(UnavailableReason::DriverLibraryMissing, error.to_string()))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn load_library() -> Result<Library> {
    Err(failure(
        UnavailableReason::Unsupported,
        "Level Zero is supported on Windows and Linux".into(),
    ))
}

pub(super) fn failure(reason: UnavailableReason, message: String) -> GpuError {
    GpuError::provider("level-zero", reason, message)
}

pub(super) fn check(code: ze_result_t) -> Result<()> {
    if code == ZE_RESULT_SUCCESS {
        return Ok(());
    }
    let reason = match code {
        ZE_RESULT_ERROR_UNSUPPORTED_FEATURE | ZE_RESULT_ERROR_UNSUPPORTED_VERSION => {
            UnavailableReason::Unsupported
        }
        ZE_RESULT_ERROR_INSUFFICIENT_PERMISSIONS => UnavailableReason::PermissionDenied,
        ZE_RESULT_ERROR_DEVICE_LOST | ZE_RESULT_ERROR_DEVICE_REQUIRES_RESET => {
            UnavailableReason::DeviceLost
        }
        ZE_RESULT_ERROR_NOT_AVAILABLE
        | ZE_RESULT_ERROR_DEVICE_IN_LOW_POWER_STATE
        | ZE_RESULT_NOT_READY => UnavailableReason::TemporarilyUnavailable,
        ZE_RESULT_ERROR_DEPENDENCY_UNAVAILABLE | ZE_RESULT_ERROR_UNINITIALIZED => {
            UnavailableReason::DriverLibraryMissing
        }
        _ => UnavailableReason::ProviderError,
    };
    Err(failure(reason, format!("Sysman returned 0x{code:08x}")))
}

/// Opaque handles have the same layout as usize on all packaged 64-bit targets.
/// Each callback is a typed C enumeration whose count parameter bounds writes.
pub(super) fn enumerate(
    mut call: impl FnMut(*mut u32, *mut usize) -> ze_result_t,
    limit: u32,
) -> Result<Vec<usize>> {
    let mut count = 0;
    check(call(&raw mut count, std::ptr::null_mut()))?;
    for _ in 0..3 {
        if count > limit {
            return Err(failure(
                UnavailableReason::ProviderError,
                "Sysman enumeration exceeded its safety limit".into(),
            ));
        }
        if count == 0 {
            return Ok(Vec::new());
        }
        let capacity = count;
        let mut handles = vec![0_usize; count as usize];
        let code = call(&raw mut count, handles.as_mut_ptr());
        if count > capacity {
            continue;
        }
        check(code)?;
        handles.truncate(count as usize);
        if handles.contains(&0) {
            return Err(failure(
                UnavailableReason::ProviderError,
                "Sysman returned a null handle".into(),
            ));
        }
        return Ok(handles);
    }
    Err(failure(
        UnavailableReason::ProviderError,
        "Sysman enumeration did not stabilize".into(),
    ))
}
