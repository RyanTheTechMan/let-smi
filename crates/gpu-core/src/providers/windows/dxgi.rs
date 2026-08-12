use crate::error::{GpuError, Result};
use crate::model::{
    CanonicalGpu, CapabilitySet, DeviceObservation, GpuKind, GpuVendor, MemoryTopology,
    PciIdentity, ProviderDiagnostic, ProviderSample, SampleRequest, StaticMemoryInfo,
    UnavailableReason, WindowsIdentity,
};
use crate::provider::{InventoryProvider, ProviderMetadata, TelemetryProvider};
use parking_lot::RwLock;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND, IDXGIFactory1,
};

pub struct DxgiProvider {
    include_software_adapters: bool,
    last_failure: RwLock<Option<String>>,
}

impl DxgiProvider {
    pub fn new(include_software_adapters: bool) -> Self {
        Self {
            include_software_adapters,
            last_failure: RwLock::new(None),
        }
    }
}

impl InventoryProvider for DxgiProvider {
    fn provider_id(&self) -> &'static str {
        "windows-dxgi"
    }

    fn enumerate(&self) -> Result<Vec<DeviceObservation>> {
        *self.last_failure.write() = None;
        // SAFETY: DXGI returns a COM interface with an owned reference and does
        // not require COM apartment initialization for factory creation.
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|error| {
            let message = format!("CreateDXGIFactory1 failed: {error}");
            *self.last_failure.write() = Some(message.clone());
            GpuError::provider(
                self.provider_id(),
                UnavailableReason::ProviderError,
                message,
            )
        })?;
        let mut result = Vec::new();
        let mut index = 0_u32;
        loop {
            // SAFETY: index is advanced until DXGI_ERROR_NOT_FOUND and the
            // returned adapter owns its COM reference.
            let adapter = match unsafe { factory.EnumAdapters1(index) } {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => {
                    *self.last_failure.write() = Some(format!("adapter {index}: {error}"));
                    break;
                }
            };
            // SAFETY: adapter is a live IDXGIAdapter1.
            let description = match unsafe { adapter.GetDesc1() } {
                Ok(description) => description,
                Err(error) => {
                    *self.last_failure.write() =
                        Some(format!("adapter {index} description: {error}"));
                    index = index.saturating_add(1);
                    continue;
                }
            };
            index = index.saturating_add(1);

            let software =
                description.Flags & u32::try_from(DXGI_ADAPTER_FLAG_SOFTWARE.0).unwrap_or(2) != 0;
            if software && !self.include_software_adapters {
                continue;
            }

            let vendor = GpuVendor::from_pci_vendor_id(description.VendorId);
            let luid = canonical_luid(
                description.AdapterLuid.HighPart,
                description.AdapterLuid.LowPart,
            );
            let mut observation = DeviceObservation::new(
                self.provider_id(),
                luid.clone(),
                vendor,
                utf16_name(&description.Description),
            );
            observation.enumeration_ordinal = u32::try_from(result.len()).ok();
            observation.identity_priority = 80;
            observation.kind = if software {
                GpuKind::Virtual
            } else {
                GpuKind::Unknown
            };
            observation.windows = Some(WindowsIdentity {
                luid: Some(luid),
                pnp_device_id: None,
            });
            observation.pci = Some(PciIdentity {
                address: None,
                vendor_id: description.VendorId,
                device_id: description.DeviceId,
                subsystem_vendor_id: (description.SubSysId != 0)
                    .then_some(description.SubSysId & 0xffff),
                subsystem_device_id: (description.SubSysId != 0)
                    .then_some(description.SubSysId >> 16),
            });

            let dedicated = u64::try_from(description.DedicatedVideoMemory).ok();
            let shared = u64::try_from(description.SharedSystemMemory).ok();
            observation.memory = StaticMemoryInfo {
                topology: match (
                    dedicated.is_some_and(|bytes| bytes > 0),
                    shared.is_some_and(|bytes| bytes > 0),
                ) {
                    (true, true) => MemoryTopology::Mixed,
                    (true, false) => MemoryTopology::Dedicated,
                    (false, true) => MemoryTopology::Shared,
                    (false, false) => MemoryTopology::Unknown,
                },
                dedicated_total_bytes: dedicated.filter(|bytes| *bytes > 0),
                shared_total_bytes: shared.filter(|bytes| *bytes > 0),
                unified_total_bytes: None,
            };
            result.push(observation);
        }
        Ok(result)
    }

    fn diagnostic(&self) -> ProviderDiagnostic {
        let failure = self.last_failure.read().clone();
        ProviderDiagnostic {
            id: self.provider_id().into(),
            loaded: failure.is_none(),
            version: None,
            devices_matched: 0,
            reason: failure.as_ref().map(|_| UnavailableReason::ProviderError),
            message: failure,
        }
    }
}

impl TelemetryProvider for DxgiProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata::new(self.provider_id(), 60, 100)
    }

    fn capabilities(&self, _device: &CanonicalGpu) -> CapabilitySet {
        CapabilitySet::default()
    }

    fn sample(&self, _device: &CanonicalGpu, _request: &SampleRequest) -> Result<ProviderSample> {
        Ok(ProviderSample::default())
    }
}

pub(super) fn canonical_luid(high: i32, low: u32) -> String {
    format!("{:08x}:{low:08x}", high as u32)
}

fn utf16_name(value: &[u16]) -> String {
    let end = value
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(value.len());
    let name = String::from_utf16_lossy(&value[..end]).trim().to_owned();
    if name.is_empty() {
        "Unknown GPU".into()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_luids_without_signed_decimal_components() {
        assert_eq!(canonical_luid(-1, 0x1234), "ffffffff:00001234");
    }

    #[test]
    fn stops_windows_names_at_nul() {
        assert_eq!(utf16_name(&[65, 77, 68, 0, 88]), "AMD");
    }
}
