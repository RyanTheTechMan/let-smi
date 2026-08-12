use crate::error::Result;
use crate::model::{
    CanonicalGpu, CapabilitySet, DeviceObservation, GpuKind, GpuVendor, MacosIdentity,
    MemoryTopology, ProviderDiagnostic, ProviderSample, SampleRequest, StaticMemoryInfo,
};
use crate::provider::{InventoryProvider, ProviderMetadata, TelemetryProvider};
use metal::{Device, MTLDeviceLocation, MTLGPUFamily};

pub struct MetalInventoryProvider;

impl InventoryProvider for MetalInventoryProvider {
    fn provider_id(&self) -> &'static str {
        "macos-metal"
    }

    fn enumerate(&self) -> Result<Vec<DeviceObservation>> {
        Ok(Device::all()
            .into_iter()
            .enumerate()
            .map(|(ordinal, device)| {
                let name = device.name().to_owned();
                let vendor = vendor_from_name(&name);
                let unified = device.has_unified_memory();
                let unified_total_bytes = unified.then(unified_memory_bytes).flatten();
                let families = supported_families(&device);
                let mut observation = DeviceObservation::new(
                    self.provider_id(),
                    format!("{:016x}", device.registry_id()),
                    vendor,
                    name.clone(),
                );
                observation.identity_priority = 80;
                observation.enumeration_ordinal = u32::try_from(ordinal).ok();
                observation.kind = kind(&device, unified);
                observation.architecture = (vendor == GpuVendor::Apple).then_some(name);
                observation.macos = Some(MacosIdentity {
                    registry_entry_id: None,
                    metal_registry_id: Some(format!("{:016x}", device.registry_id())),
                });
                observation.memory = StaticMemoryInfo {
                    topology: if unified {
                        MemoryTopology::Unified
                    } else {
                        MemoryTopology::Dedicated
                    },
                    unified_total_bytes,
                    ..StaticMemoryInfo::default()
                };
                observation.vendor_info.insert(
                    "metalRegistryId".into(),
                    serde_json::json!(format!("{:016x}", device.registry_id())),
                );
                observation.vendor_info.insert(
                    "recommendedMaxWorkingSetBytes".into(),
                    serde_json::json!(device.recommended_max_working_set_size()),
                );
                observation
                    .vendor_info
                    .insert("metalGpuFamilies".into(), serde_json::json!(families));
                if vendor == GpuVendor::Apple {
                    if let Some(family) = families
                        .iter()
                        .rev()
                        .find(|family| family.starts_with("apple"))
                    {
                        observation
                            .vendor_info
                            .insert("gpuFamily".into(), serde_json::json!(family));
                    }
                    if let Some(bytes) = unified_total_bytes {
                        observation
                            .vendor_info
                            .insert("unifiedMemoryTotalBytes".into(), serde_json::json!(bytes));
                    }
                }
                observation
            })
            .collect())
    }

    fn diagnostic(&self) -> ProviderDiagnostic {
        ProviderDiagnostic {
            id: self.provider_id().into(),
            loaded: true,
            version: None,
            devices_matched: Device::all().len(),
            reason: None,
            message: None,
        }
    }
}

impl TelemetryProvider for MetalInventoryProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata::new(self.provider_id(), 70, 100)
    }

    fn capabilities(&self, _device: &CanonicalGpu) -> CapabilitySet {
        CapabilitySet::default()
    }

    fn sample(&self, _device: &CanonicalGpu, _request: &SampleRequest) -> Result<ProviderSample> {
        Ok(ProviderSample::default())
    }
}

fn vendor_from_name(name: &str) -> GpuVendor {
    let lowercase = name.to_ascii_lowercase();
    if lowercase.contains("apple") {
        GpuVendor::Apple
    } else if lowercase.contains("nvidia") || lowercase.contains("geforce") {
        GpuVendor::Nvidia
    } else if lowercase.contains("amd") || lowercase.contains("radeon") {
        GpuVendor::Amd
    } else if lowercase.contains("intel") {
        GpuVendor::Intel
    } else {
        GpuVendor::Unknown
    }
}

fn kind(device: &metal::DeviceRef, unified: bool) -> GpuKind {
    if device.is_removable() || device.location() == MTLDeviceLocation::External {
        GpuKind::External
    } else if unified || device.is_low_power() {
        GpuKind::Integrated
    } else if device.location() == MTLDeviceLocation::Slot {
        GpuKind::Discrete
    } else {
        GpuKind::Unknown
    }
}

fn supported_families(device: &metal::DeviceRef) -> Vec<&'static str> {
    [
        (MTLGPUFamily::Apple1, "apple1"),
        (MTLGPUFamily::Apple2, "apple2"),
        (MTLGPUFamily::Apple3, "apple3"),
        (MTLGPUFamily::Apple4, "apple4"),
        (MTLGPUFamily::Apple5, "apple5"),
        (MTLGPUFamily::Apple6, "apple6"),
        (MTLGPUFamily::Apple7, "apple7"),
        (MTLGPUFamily::Apple8, "apple8"),
        (MTLGPUFamily::Apple9, "apple9"),
        (MTLGPUFamily::Mac1, "mac1"),
        (MTLGPUFamily::Mac2, "mac2"),
        (MTLGPUFamily::Metal3, "metal3"),
        (MTLGPUFamily::Metal4, "metal4"),
    ]
    .into_iter()
    .filter_map(|(family, name)| device.supports_family(family).then_some(name))
    .collect()
}

fn unified_memory_bytes() -> Option<u64> {
    let mut value = 0_u64;
    let mut size = std::mem::size_of::<u64>();
    let name = c"hw.memsize";
    // SAFETY: The output points to a correctly sized u64 and sysctlbyname does
    // not retain either pointer.
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut value).cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0 && size == std::mem::size_of::<u64>()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_common_legacy_vendors() {
        assert_eq!(vendor_from_name("AMD Radeon Pro 5500M"), GpuVendor::Amd);
        assert_eq!(vendor_from_name("Intel Iris Plus"), GpuVendor::Intel);
        assert_eq!(vendor_from_name("Apple M5 Max"), GpuVendor::Apple);
    }

    #[test]
    fn reads_unified_memory_without_a_process() {
        assert!(unified_memory_bytes().is_some_and(|bytes| bytes > 0));
    }
}
