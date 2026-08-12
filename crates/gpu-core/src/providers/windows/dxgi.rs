use crate::error::{GpuError, Result};
use crate::model::{
    CanonicalGpu, CapabilitySet, DeviceObservation, GpuKind, GpuVendor, MemoryTopology,
    PciIdentity, ProviderDiagnostic, ProviderSample, SampleRequest, StaticMemoryInfo,
    UnavailableReason, WindowsIdentity,
};
use crate::provider::{InventoryProvider, ProviderMetadata, TelemetryProvider};
use parking_lot::RwLock;
use std::mem::size_of;
use windows::Wdk::Graphics::Direct3D::{
    D3DKMT_ADAPTERADDRESS, D3DKMT_ADAPTERTYPE, D3DKMT_CLOSEADAPTER, D3DKMT_OPENADAPTERFROMLUID,
    D3DKMT_QUERYADAPTERINFO, D3DKMTCloseAdapter, D3DKMTOpenAdapterFromLuid, D3DKMTQueryAdapterInfo,
    KMTQAITYPE_ADAPTERADDRESS, KMTQAITYPE_ADAPTERTYPE,
};
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND, IDXGIFactory1,
};

const INVALID_PCI_BUS: u32 = u32::MAX;
const INVALID_PCI_DEVICE_OR_FUNCTION: u32 = u16::MAX as u32;
const ADAPTER_TYPE_SOFTWARE_DEVICE: u32 = 1 << 2;
const ADAPTER_TYPE_HYBRID_DISCRETE: u32 = 1 << 4;
const ADAPTER_TYPE_HYBRID_INTEGRATED: u32 = 1 << 5;
const MAX_DXGI_ADAPTERS: u32 = 256;

pub struct DxgiProvider {
    include_software_adapters: bool,
    last_failure: RwLock<Option<String>>,
    last_warnings: RwLock<Vec<String>>,
}

impl DxgiProvider {
    pub fn new(include_software_adapters: bool) -> Self {
        Self {
            include_software_adapters,
            last_failure: RwLock::new(None),
            last_warnings: RwLock::new(Vec::new()),
        }
    }
}

impl InventoryProvider for DxgiProvider {
    fn provider_id(&self) -> &'static str {
        "windows-dxgi"
    }

    fn enumerate(&self) -> Result<Vec<DeviceObservation>> {
        *self.last_failure.write() = None;
        self.last_warnings.write().clear();
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
            if index >= MAX_DXGI_ADAPTERS {
                self.last_warnings.write().push(format!(
                    "DXGI adapter enumeration stopped at the {MAX_DXGI_ADAPTERS}-adapter safety limit"
                ));
                break;
            }
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
                    index += 1;
                    continue;
                }
            };
            index += 1;

            let software =
                description.Flags & u32::try_from(DXGI_ADAPTER_FLAG_SOFTWARE.0).unwrap_or(2) != 0;
            if software && !self.include_software_adapters {
                continue;
            }

            let pci_address = match pci_address_for_luid(description.AdapterLuid) {
                Ok(address) => Some(address),
                Err(PciAddressError::NotBackedByPci) => {
                    if software {
                        None
                    } else {
                        // Hybrid drivers can expose render-only presentation
                        // paths in DXGI. D3DKMT marks these with invalid PCI
                        // sentinels; they are not separate physical adapters.
                        continue;
                    }
                }
                Err(PciAddressError::Query(message)) => {
                    self.last_warnings
                        .write()
                        .push(format!("adapter {} PCI enrichment: {message}", index - 1));
                    None
                }
            };

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
            observation.kind =
                adapter_kind(description.AdapterLuid, software).unwrap_or_else(|message| {
                    self.last_warnings
                        .write()
                        .push(format!("adapter {} kind enrichment: {message}", index - 1));
                    if software {
                        GpuKind::Virtual
                    } else {
                        GpuKind::Unknown
                    }
                });
            observation.windows = Some(WindowsIdentity {
                luid: Some(luid),
                pnp_device_id: None,
            });
            observation.pci = Some(PciIdentity {
                address: pci_address,
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
        let warnings = self.last_warnings.read();
        ProviderDiagnostic {
            id: self.provider_id().into(),
            loaded: failure.is_none(),
            version: None,
            devices_matched: 0,
            reason: failure.as_ref().map(|_| UnavailableReason::ProviderError),
            message: failure.or_else(|| {
                (!warnings.is_empty()).then(|| {
                    format!(
                        "DXGI inventory succeeded with {} non-fatal warning(s): {}",
                        warnings.len(),
                        warnings.join("; ")
                    )
                })
            }),
        }
    }
}

struct D3dkmtAdapter(u32);

impl Drop for D3dkmtAdapter {
    fn drop(&mut self) {
        if self.0 != 0 {
            let close = D3DKMT_CLOSEADAPTER { hAdapter: self.0 };
            // SAFETY: this handle was returned by D3DKMTOpenAdapterFromLuid and
            // is closed exactly once when the guard drops.
            unsafe {
                let _ = D3DKMTCloseAdapter(&close);
            }
        }
    }
}

enum PciAddressError {
    NotBackedByPci,
    Query(String),
}

fn open_d3dkmt_adapter(luid: LUID) -> std::result::Result<D3dkmtAdapter, String> {
    let mut open = D3DKMT_OPENADAPTERFROMLUID {
        AdapterLuid: luid,
        hAdapter: 0,
    };
    // SAFETY: `open` is an initialized in/out structure containing the DXGI
    // LUID and remains live for the duration of the call.
    let status = unsafe { D3DKMTOpenAdapterFromLuid(&raw mut open) };
    if status.0 < 0 || open.hAdapter == 0 {
        return Err(format!(
            "D3DKMTOpenAdapterFromLuid failed with NTSTATUS 0x{:08x}",
            status.0 as u32
        ));
    }
    Ok(D3dkmtAdapter(open.hAdapter))
}

fn pci_address_for_luid(luid: LUID) -> std::result::Result<String, PciAddressError> {
    let adapter = open_d3dkmt_adapter(luid).map_err(PciAddressError::Query)?;
    let mut address = D3DKMT_ADAPTERADDRESS::default();
    let mut query = D3DKMT_QUERYADAPTERINFO {
        hAdapter: adapter.0,
        Type: KMTQAITYPE_ADAPTERADDRESS,
        pPrivateDriverData: (&raw mut address).cast(),
        PrivateDriverDataSize: u32::try_from(size_of::<D3DKMT_ADAPTERADDRESS>()).map_err(|_| {
            PciAddressError::Query("D3DKMT adapter-address structure is too large".to_owned())
        })?,
    };
    // SAFETY: the adapter handle is live, and the private data pointer and size
    // exactly describe the initialized D3DKMT_ADAPTERADDRESS output buffer.
    let status = unsafe { D3DKMTQueryAdapterInfo(&raw mut query) };
    if status.0 < 0 {
        return Err(PciAddressError::Query(format!(
            "D3DKMTQueryAdapterInfo(KMTQAITYPE_ADAPTERADDRESS) failed with NTSTATUS 0x{:08x}",
            status.0 as u32
        )));
    }
    if address.BusNumber == INVALID_PCI_BUS
        && address.DeviceNumber == INVALID_PCI_DEVICE_OR_FUNCTION
        && address.FunctionNumber == INVALID_PCI_DEVICE_OR_FUNCTION
    {
        return Err(PciAddressError::NotBackedByPci);
    }
    canonical_pci_location(
        address.BusNumber,
        address.DeviceNumber,
        address.FunctionNumber,
    )
    .ok_or_else(|| {
        PciAddressError::Query(format!(
            "D3DKMT returned an invalid PCI location ({}, {}, {})",
            address.BusNumber, address.DeviceNumber, address.FunctionNumber
        ))
    })
}

fn adapter_kind(luid: LUID, dxgi_software: bool) -> std::result::Result<GpuKind, String> {
    let adapter = open_d3dkmt_adapter(luid)?;
    let mut adapter_type = D3DKMT_ADAPTERTYPE::default();
    let mut query = D3DKMT_QUERYADAPTERINFO {
        hAdapter: adapter.0,
        Type: KMTQAITYPE_ADAPTERTYPE,
        pPrivateDriverData: (&raw mut adapter_type).cast(),
        PrivateDriverDataSize: u32::try_from(size_of::<D3DKMT_ADAPTERTYPE>())
            .map_err(|_| "D3DKMT adapter-type structure is too large".to_owned())?,
    };
    // SAFETY: the adapter handle is live, and the private data pointer and size
    // exactly describe the initialized D3DKMT_ADAPTERTYPE output buffer.
    let status = unsafe { D3DKMTQueryAdapterInfo(&raw mut query) };
    if status.0 < 0 {
        return Err(format!(
            "D3DKMTQueryAdapterInfo(KMTQAITYPE_ADAPTERTYPE) failed with NTSTATUS 0x{:08x}",
            status.0 as u32
        ));
    }
    // SAFETY: D3DKMT populated the union, and Value covers the full C union.
    let flags = unsafe { adapter_type.Anonymous.Value };
    Ok(adapter_kind_from_flags(flags, dxgi_software))
}

fn adapter_kind_from_flags(flags: u32, dxgi_software: bool) -> GpuKind {
    if dxgi_software || flags & ADAPTER_TYPE_SOFTWARE_DEVICE != 0 {
        GpuKind::Virtual
    } else if flags & ADAPTER_TYPE_HYBRID_INTEGRATED != 0 {
        GpuKind::Integrated
    } else if flags & ADAPTER_TYPE_HYBRID_DISCRETE != 0 {
        GpuKind::Discrete
    } else {
        GpuKind::Unknown
    }
}

fn canonical_pci_location(bus: u32, device: u32, function: u32) -> Option<String> {
    (bus <= 0xff && device <= 0x1f && function <= 7)
        .then(|| format!("0000:{bus:02x}:{device:02x}.{function:x}"))
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

    #[test]
    fn validates_and_formats_d3dkmt_pci_locations() {
        assert_eq!(
            canonical_pci_location(1, 2, 3).as_deref(),
            Some("0000:01:02.3")
        );
        assert!(canonical_pci_location(256, 0, 0).is_none());
        assert!(canonical_pci_location(0, 32, 0).is_none());
        assert!(canonical_pci_location(0, 0, 8).is_none());
    }

    #[test]
    fn interprets_d3dkmt_adapter_kind_flags() {
        assert_eq!(
            adapter_kind_from_flags(ADAPTER_TYPE_HYBRID_INTEGRATED, false),
            GpuKind::Integrated
        );
        assert_eq!(
            adapter_kind_from_flags(ADAPTER_TYPE_HYBRID_DISCRETE, false),
            GpuKind::Discrete
        );
        assert_eq!(adapter_kind_from_flags(0, true), GpuKind::Virtual);
    }

    #[test]
    fn adapter_enumeration_has_a_finite_upper_bound() {
        assert_eq!(MAX_DXGI_ADAPTERS, 256);
    }
}
