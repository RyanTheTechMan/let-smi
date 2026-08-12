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
    reason: UnavailableReason,
    message: String,
}

impl WindowsRuntimeProbe {
    pub fn system32(id: &'static str, library: &str, detected_message: &str) -> Self {
        if !safe_system32_library_name(library) {
            return Self {
                id,
                reason: UnavailableReason::ProviderError,
                message: "optional Windows runtime probe rejected an unsafe library name".into(),
            };
        }
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
                    reason: UnavailableReason::Unsupported,
                    message: detected_message.into(),
                }
            }
            Err(error) => Self {
                id,
                reason: UnavailableReason::DriverLibraryMissing,
                message: format!("{library} is not installed: {error}"),
            },
        }
    }
}

fn safe_system32_library_name(library: &str) -> bool {
    library.len() <= 128
        && library
            .strip_suffix(".dll")
            .or_else(|| library.strip_suffix(".DLL"))
            .is_some_and(|stem| {
                !stem.is_empty()
                    && stem.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
                    && !stem.contains("..")
            })
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
            // Presence probes are diagnostic boundaries, not functional
            // providers. In particular, requiredProviders must not accept a
            // DLL whose device adapter and telemetry ABI are unimplemented.
            loaded: false,
            version: None,
            devices_matched: 0,
            reason: Some(self.reason),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detected_runtime_is_not_a_functional_provider() {
        let probe = WindowsRuntimeProbe {
            id: "fixture",
            reason: UnavailableReason::Unsupported,
            message: "detected but unimplemented".into(),
        };
        let diagnostic = probe.diagnostic();
        assert!(!diagnostic.loaded);
        assert_eq!(diagnostic.reason, Some(UnavailableReason::Unsupported));
        assert!(
            probe
                .capabilities(&fixture_gpu())
                .metrics()
                .next()
                .is_none()
        );
    }

    #[test]
    fn system32_probe_rejects_qualified_or_malformed_names() {
        assert!(safe_system32_library_name("ze_loader.dll"));
        assert!(safe_system32_library_name("atiadlxx.dll"));
        assert!(!safe_system32_library_name(r"C:\untrusted\ze_loader.dll"));
        assert!(!safe_system32_library_name(r"..\ze_loader.dll"));
        assert!(!safe_system32_library_name("ze_loader"));
        assert!(!safe_system32_library_name("evil\0.dll"));
    }

    fn fixture_gpu() -> CanonicalGpu {
        crate::correlation::correlate(vec![DeviceObservation::new(
            "fixture",
            "device",
            crate::model::GpuVendor::Unknown,
            "GPU",
        )])
        .pop()
        .expect("fixture GPU")
    }
}
