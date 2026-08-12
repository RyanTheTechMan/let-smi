use crate::error::Result;
use crate::model::{
    CanonicalGpu, CapabilitySet, DeviceObservation, ProviderDiagnostic, ProviderSample,
    SampleRequest, UnavailableReason,
};
use crate::provider::{InventoryProvider, ProviderMetadata, TelemetryProvider};
use windows::Win32::Foundation::FreeLibrary;
use windows::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::core::HSTRING;

pub struct WindowsRuntimeProbe {
    id: &'static str,
    loaded: bool,
    message: String,
}

impl WindowsRuntimeProbe {
    pub fn system32(id: &'static str, library: &str, detected_message: &str) -> Self {
        let name = HSTRING::from(library);
        // SAFETY: LOAD_LIBRARY_SEARCH_SYSTEM32 prevents current-directory DLL
        // injection and the returned module is released immediately after the
        // presence probe.
        let result = unsafe { LoadLibraryExW(&name, None, LOAD_LIBRARY_SEARCH_SYSTEM32) };
        match result {
            Ok(module) => {
                // SAFETY: module was returned by LoadLibraryExW and is not used
                // after this call.
                unsafe {
                    let _ = FreeLibrary(module);
                }
                Self {
                    id,
                    loaded: true,
                    message: detected_message.into(),
                }
            }
            Err(error) => Self {
                id,
                loaded: false,
                message: format!("{library} is not installed: {error}"),
            },
        }
    }
}

impl InventoryProvider for WindowsRuntimeProbe {
    fn provider_id(&self) -> &'static str {
        self.id
    }

    fn enumerate(&self) -> Result<Vec<DeviceObservation>> {
        Ok(Vec::new())
    }

    fn diagnostic(&self) -> ProviderDiagnostic {
        ProviderDiagnostic {
            id: self.id.into(),
            loaded: self.loaded,
            version: None,
            devices_matched: 0,
            reason: (!self.loaded).then_some(UnavailableReason::DriverLibraryMissing),
            message: Some(self.message.clone()),
        }
    }
}

impl TelemetryProvider for WindowsRuntimeProbe {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata::new(self.id, 0, 0)
    }

    fn capabilities(&self, _device: &CanonicalGpu) -> CapabilitySet {
        CapabilitySet::default()
    }

    fn sample(&self, _device: &CanonicalGpu, _request: &SampleRequest) -> Result<ProviderSample> {
        Ok(ProviderSample::default())
    }
}
