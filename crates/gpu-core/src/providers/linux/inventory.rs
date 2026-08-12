use super::hwmon;
use super::metric::{MetricReader, MetricSource};
use super::parse::{drm_node_name, parse_u64, read_text, uevent_value};
use super::roots::LinuxRoots;
use crate::model::{
    CapabilitySet, DeviceObservation, GpuKind, GpuVendor, MemoryTopology, MetricKey, MetricQuality,
    PartitionIdentity, PartitionType, PciIdentity, StaticMemoryInfo,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub(crate) const PROVIDER_ID: &str = "linux-sysfs";

#[derive(Debug, Clone)]
pub(crate) struct LinuxDeviceRecord {
    pub provider_device_id: String,
    pub device_path: PathBuf,
    pub observation: DeviceObservation,
    pub metric_sources: BTreeMap<MetricKey, Vec<MetricSource>>,
}

#[derive(Debug, Default)]
pub(crate) struct InventoryResult {
    pub records: Vec<LinuxDeviceRecord>,
    pub warnings: Vec<String>,
    pub sysfs_available: bool,
}

#[derive(Debug, Default)]
struct DeviceBuilder {
    device_path: PathBuf,
    pci_address: Option<String>,
    drm_nodes: BTreeSet<String>,
}

pub(crate) fn discover(roots: &LinuxRoots, include_software_adapters: bool) -> InventoryResult {
    let mut result = InventoryResult::default();
    let mut devices: BTreeMap<PathBuf, DeviceBuilder> = BTreeMap::new();

    match fs::read_dir(roots.pci_devices()) {
        Ok(entries) => {
            result.sysfs_available = true;
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                let Some(class) = read_optional_u64(&path.join("class")) else {
                    continue;
                };
                if (class >> 16) != 0x03 {
                    continue;
                }
                let canonical = canonical_or_self(&path);
                let pci_address = entry.file_name().to_string_lossy().into_owned();
                let builder = devices
                    .entry(canonical.clone())
                    .or_insert_with(|| DeviceBuilder {
                        device_path: canonical,
                        ..DeviceBuilder::default()
                    });
                builder.pci_address = Some(pci_address);
            }
        }
        Err(error) if error.kind() != io::ErrorKind::NotFound => result.warnings.push(format!(
            "could not enumerate {}: {error}",
            roots.pci_devices().display()
        )),
        Err(_) => {}
    }

    match fs::read_dir(roots.drm_class()) {
        Ok(entries) => {
            result.sysfs_available = true;
            for entry in entries.filter_map(Result::ok) {
                let node = entry.file_name().to_string_lossy().into_owned();
                if !drm_node_name(&node) {
                    continue;
                }
                let device_link = entry.path().join("device");
                let Ok(canonical) = fs::canonicalize(&device_link) else {
                    continue;
                };
                let builder = devices
                    .entry(canonical.clone())
                    .or_insert_with(|| DeviceBuilder {
                        device_path: canonical,
                        ..DeviceBuilder::default()
                    });
                builder.drm_nodes.insert(node);
            }
        }
        Err(error) if error.kind() != io::ErrorKind::NotFound => result.warnings.push(format!(
            "could not enumerate {}: {error}",
            roots.drm_class().display()
        )),
        Err(_) => {}
    }

    for builder in devices.into_values() {
        if let Some(record) = build_record(roots, builder, include_software_adapters) {
            result.records.push(record);
        }
    }
    result
        .records
        .sort_by(|left, right| left.provider_device_id.cmp(&right.provider_device_id));
    result
}

fn build_record(
    roots: &LinuxRoots,
    builder: DeviceBuilder,
    include_software_adapters: bool,
) -> Option<LinuxDeviceRecord> {
    let device_path = builder.device_path;
    let driver = driver_name(&device_path);
    if !include_software_adapters && driver.as_deref().is_some_and(is_software_driver) {
        return None;
    }

    let pci = pci_identity(&device_path, builder.pci_address.as_deref());
    let vendor = pci.as_ref().map_or_else(
        || vendor_from_driver(driver.as_deref()),
        |pci| GpuVendor::from_pci_vendor_id(pci.vendor_id),
    );
    let provider_device_id = pci
        .as_ref()
        .and_then(|pci| pci.address.clone())
        .unwrap_or_else(|| platform_device_id(roots, &device_path));

    let mut metric_sources =
        discover_device_sources(&device_path, vendor, driver.as_deref(), &builder.drm_nodes);
    for sources in metric_sources.values_mut() {
        sources.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.definition.cmp(&right.definition))
        });
    }

    let memory = static_memory(&device_path);
    let mut capabilities = CapabilitySet::default();
    for (key, sources) in &metric_sources {
        if sources.iter().any(|source| source.probe(&device_path)) {
            capabilities.insert(*key);
        }
    }

    let mut observation = DeviceObservation::new(
        PROVIDER_ID,
        &provider_device_id,
        vendor,
        gpu_name(&device_path, vendor, pci.as_ref()),
    );
    observation.pci = pci;
    observation.kind = gpu_kind(driver.as_deref());
    observation.driver_version = driver_version(&device_path);
    observation.firmware_version = read_text(&device_path.join("vbios_version"))
        .ok()
        .filter(|value| !value.is_empty());
    observation.memory = memory;
    observation.capabilities = capabilities;
    observation.identity_priority = 60;
    observation.parent_provider_device_id = parent_device_id(&device_path);
    if observation.parent_provider_device_id.is_some() {
        observation.partition = Some(PartitionIdentity {
            partition_type: PartitionType::Sriov,
            id: provider_device_id.clone(),
        });
    }

    let sysfs_path = device_path
        .strip_prefix(canonical_or_self(&roots.sys))
        .unwrap_or(&device_path)
        .display()
        .to_string();
    observation.vendor_info.insert(
        "linux".into(),
        json!({
            "driver": driver,
            "drmNodes": builder.drm_nodes,
            "sysfsPath": sysfs_path,
        }),
    );
    if vendor == GpuVendor::Amd {
        if let Some(bytes) = observation.memory.dedicated_total_bytes {
            observation
                .vendor_info
                .insert("vramTotalBytes".into(), json!(bytes));
        }
        if let Some(bytes) = observation.memory.shared_total_bytes {
            observation
                .vendor_info
                .insert("gttTotalBytes".into(), json!(bytes));
        }
    }

    Some(LinuxDeviceRecord {
        provider_device_id,
        device_path,
        observation,
        metric_sources,
    })
}

fn discover_device_sources(
    device_path: &Path,
    vendor: GpuVendor,
    driver: Option<&str>,
    drm_nodes: &BTreeSet<String>,
) -> BTreeMap<MetricKey, Vec<MetricSource>> {
    let mut sources = hwmon::discover(device_path, vendor);

    if vendor == GpuVendor::Amd || driver == Some("amdgpu") {
        add_if_present(
            &mut sources,
            MetricSource::new(
                MetricKey::UtilizationOverall,
                MetricReader::Percent(device_path.join("gpu_busy_percent")),
                MetricQuality::Direct,
                "AMD SMU-reported GPU busy percentage",
                200,
            ),
        );
        add_if_present(
            &mut sources,
            MetricSource::new(
                MetricKey::MemoryBandwidthUtilizationPercent,
                MetricReader::Percent(device_path.join("mem_busy_percent")),
                MetricQuality::Direct,
                "AMD SMU-reported memory-controller busy percentage",
                200,
            ),
        );
        add_if_present(
            &mut sources,
            MetricSource::new(
                MetricKey::MemoryDedicatedUsedBytes,
                MetricReader::Bytes(device_path.join("mem_info_vram_used")),
                MetricQuality::Direct,
                "AMD kernel-reported VRAM usage in bytes",
                200,
            ),
        );
        add_if_present(
            &mut sources,
            MetricSource::new(
                MetricKey::MemorySharedUsedBytes,
                MetricReader::Bytes(device_path.join("mem_info_gtt_used")),
                MetricQuality::Direct,
                "AMD kernel-reported GTT usage in bytes",
                200,
            ),
        );
        add_if_present(
            &mut sources,
            MetricSource::new(
                MetricKey::ClockGraphicsMhz,
                MetricReader::ActiveDpmMhz(device_path.join("pp_dpm_sclk")),
                MetricQuality::Direct,
                "active AMD graphics DPM state clock in MHz",
                200,
            ),
        );
        add_if_present(
            &mut sources,
            MetricSource::new(
                MetricKey::ClockMemoryMhz,
                MetricReader::ActiveDpmMhz(device_path.join("pp_dpm_mclk")),
                MetricQuality::Direct,
                "active AMD memory DPM state clock in MHz",
                200,
            ),
        );
    }

    if driver == Some("i915") {
        let mut frequency_paths = Vec::new();
        let legacy_device_path = device_path.join("gt_cur_freq_mhz");
        if legacy_device_path.exists() {
            frequency_paths.push(legacy_device_path);
        }
        for card in drm_nodes.iter().filter(|node| node.starts_with("card")) {
            let card_path = device_path.join("drm").join(card);
            let legacy_path = card_path.join("gt_cur_freq_mhz");
            if legacy_path.exists() {
                frequency_paths.push(legacy_path);
            }
            for gt in read_child_directories(&card_path.join("gt"), "gt") {
                let path = gt.join("rps_cur_freq_mhz");
                if path.exists() {
                    frequency_paths.push(path);
                }
            }
        }
        frequency_paths.sort();
        frequency_paths.dedup();
        if !frequency_paths.is_empty() {
            sources.push(MetricSource::new(
                MetricKey::ClockGraphicsMhz,
                MetricReader::MaximumMhz(frequency_paths),
                MetricQuality::Direct,
                "maximum current Intel i915 GT frequency in MHz",
                180,
            ));
        }
    }

    if driver == Some("xe") {
        let frequency_paths = xe_frequency_paths(device_path);
        if !frequency_paths.is_empty() {
            sources.push(MetricSource::new(
                MetricKey::ClockGraphicsMhz,
                MetricReader::MaximumMhz(frequency_paths),
                MetricQuality::Direct,
                "maximum current Intel Xe GT frequency across tiles in MHz",
                180,
            ));
        }
    }

    let mut grouped: BTreeMap<MetricKey, Vec<MetricSource>> = BTreeMap::new();
    for source in sources {
        grouped.entry(source.key).or_default().push(source);
    }
    grouped
}

fn add_if_present(sources: &mut Vec<MetricSource>, source: MetricSource) {
    if source.exists() {
        sources.push(source);
    }
}

fn xe_frequency_paths(device_path: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for tile in read_child_directories(device_path, "tile") {
        for gt in read_child_directories(&tile, "gt") {
            let path = gt.join("freq0/act_freq");
            if path.exists() {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths
}

fn read_child_directories(parent: &Path, prefix: &str) -> Vec<PathBuf> {
    fs::read_dir(parent)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            let suffix = name.strip_prefix(prefix)?;
            if suffix.is_empty() || !suffix.chars().all(|character| character.is_ascii_digit()) {
                return None;
            }
            entry
                .file_type()
                .ok()
                .filter(|kind| kind.is_dir() || kind.is_symlink())
                .map(|_| entry.path())
        })
        .collect()
}

fn pci_identity(device_path: &Path, address: Option<&str>) -> Option<PciIdentity> {
    let vendor_id = read_optional_u64(&device_path.join("vendor"))?;
    let device_id = read_optional_u64(&device_path.join("device"))?;
    Some(PciIdentity {
        address: address.map(str::to_owned).or_else(|| {
            device_path.file_name().and_then(|name| {
                let name = name.to_string_lossy();
                looks_like_pci_address(&name).then(|| name.into_owned())
            })
        }),
        vendor_id: u32::try_from(vendor_id).ok()?,
        device_id: u32::try_from(device_id).ok()?,
        subsystem_vendor_id: read_optional_u64(&device_path.join("subsystem_vendor"))
            .and_then(|value| u32::try_from(value).ok()),
        subsystem_device_id: read_optional_u64(&device_path.join("subsystem_device"))
            .and_then(|value| u32::try_from(value).ok()),
    })
}

fn looks_like_pci_address(value: &str) -> bool {
    let mut parts = value.split([':', '.']);
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(domain), Some(bus), Some(device), Some(function), None)
            if domain.len() == 4
                && bus.len() == 2
                && device.len() == 2
                && function.len() == 1
                && [domain, bus, device, function]
                    .into_iter()
                    .all(|part| part.chars().all(|character| character.is_ascii_hexdigit()))
    )
}

fn static_memory(device_path: &Path) -> StaticMemoryInfo {
    let dedicated_total_bytes = read_optional_u64(&device_path.join("mem_info_vram_total"));
    let shared_total_bytes = read_optional_u64(&device_path.join("mem_info_gtt_total"));
    let topology = match (dedicated_total_bytes, shared_total_bytes) {
        (Some(dedicated), Some(shared)) if dedicated > 0 && shared > 0 => MemoryTopology::Mixed,
        (Some(dedicated), _) if dedicated > 0 => MemoryTopology::Dedicated,
        (_, Some(shared)) if shared > 0 => MemoryTopology::Shared,
        _ => MemoryTopology::Unknown,
    };
    StaticMemoryInfo {
        topology,
        dedicated_total_bytes,
        shared_total_bytes,
        unified_total_bytes: None,
    }
}

fn parent_device_id(device_path: &Path) -> Option<String> {
    let parent = fs::canonicalize(device_path.join("physfn")).ok()?;
    let name = parent.file_name()?.to_string_lossy();
    looks_like_pci_address(&name).then(|| name.into_owned())
}

fn driver_name(device_path: &Path) -> Option<String> {
    canonical_basename(&device_path.join("driver/module"))
        .or_else(|| canonical_basename(&device_path.join("driver")))
        .or_else(|| {
            let uevent = read_text(&device_path.join("uevent")).ok()?;
            uevent_value(&uevent, "DRIVER").map(str::to_owned)
        })
}

fn canonical_basename(path: &Path) -> Option<String> {
    fs::canonicalize(path).ok().and_then(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    })
}

fn driver_version(device_path: &Path) -> Option<String> {
    read_text(&device_path.join("driver/module/version"))
        .ok()
        .filter(|value| !value.is_empty())
}

fn vendor_from_driver(driver: Option<&str>) -> GpuVendor {
    match driver {
        Some("amdgpu" | "radeon") => GpuVendor::Amd,
        Some("i915" | "xe") => GpuVendor::Intel,
        Some("nvidia" | "nouveau") => GpuVendor::Nvidia,
        Some("asahi") => GpuVendor::Apple,
        _ => GpuVendor::Unknown,
    }
}

fn gpu_kind(driver: Option<&str>) -> GpuKind {
    match driver {
        Some("virtio_gpu" | "qxl" | "vmwgfx" | "bochs") => GpuKind::Virtual,
        Some("asahi") => GpuKind::Integrated,
        _ => GpuKind::Unknown,
    }
}

fn is_software_driver(driver: &str) -> bool {
    matches!(
        driver,
        "simpledrm" | "simple-framebuffer" | "efi-framebuffer" | "vkms" | "vgem"
    )
}

fn gpu_name(device_path: &Path, vendor: GpuVendor, pci: Option<&PciIdentity>) -> String {
    if let Some(product_name) = read_text(&device_path.join("product_name"))
        .ok()
        .filter(|value| !value.is_empty())
    {
        return product_name;
    }
    let vendor_name = match vendor {
        GpuVendor::Nvidia => "NVIDIA",
        GpuVendor::Amd => "AMD",
        GpuVendor::Intel => "Intel",
        GpuVendor::Apple => "Apple",
        GpuVendor::Unknown => "Unknown",
    };
    pci.map_or_else(
        || format!("{vendor_name} GPU"),
        |pci| format!("{vendor_name} GPU 0x{:04x}", pci.device_id),
    )
}

fn platform_device_id(roots: &LinuxRoots, device_path: &Path) -> String {
    let sys_root = canonical_or_self(&roots.sys);
    let relative = device_path.strip_prefix(sys_root).unwrap_or(device_path);
    format!("platform:{}", relative.display())
}

fn read_optional_u64(path: &Path) -> Option<u64> {
    read_text(path)
        .ok()
        .and_then(|value| parse_u64(&value).ok())
}

fn canonical_or_self(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

pub(crate) fn vendor_info(record: &LinuxDeviceRecord) -> Value {
    let linux = record
        .observation
        .vendor_info
        .get("linux")
        .cloned()
        .unwrap_or(Value::Null);
    json!({ "linux": linux })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture {
        root: PathBuf,
        roots: LinuxRoots,
    }

    impl Fixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "let-smi-linux-inventory-{}-{nonce}",
                std::process::id()
            ));
            let roots = LinuxRoots::new(root.join("sys"), root.join("proc"), root.join("dev"));
            fs::create_dir_all(roots.pci_devices()).expect("PCI fixture root");
            fs::create_dir_all(roots.drm_class()).expect("DRM fixture root");
            Self { root, roots }
        }

        fn amd_gpu(&self) -> PathBuf {
            let device = self.roots.sys.join("devices/pci0000:00/0000:03:00.0");
            fs::create_dir_all(device.join("drm/card0")).expect("device directories");
            fs::create_dir_all(device.join("hwmon/hwmon0")).expect("hwmon directory");
            write(&device.join("class"), "0x030000\n");
            write(&device.join("vendor"), "0x1002\n");
            write(&device.join("device"), "0x744c\n");
            write(&device.join("subsystem_vendor"), "0x1da2\n");
            write(&device.join("subsystem_device"), "0xe471\n");
            write(&device.join("uevent"), "DRIVER=amdgpu\n");
            write(&device.join("gpu_busy_percent"), "42\n");
            write(&device.join("mem_busy_percent"), "13\n");
            write(&device.join("mem_info_vram_total"), "25769803776\n");
            write(&device.join("mem_info_vram_used"), "4294967296\n");
            write(&device.join("mem_info_gtt_total"), "17179869184\n");
            write(&device.join("mem_info_gtt_used"), "1073741824\n");
            write(&device.join("pp_dpm_sclk"), "0: 500Mhz\n1: 2100Mhz *\n");
            write(&device.join("pp_dpm_mclk"), "0: 96Mhz\n1: 1249Mhz *\n");
            write(&device.join("hwmon/hwmon0/name"), "amdgpu\n");
            write(&device.join("hwmon/hwmon0/temp1_input"), "51000\n");
            write(&device.join("hwmon/hwmon0/temp1_label"), "edge\n");
            write(&device.join("hwmon/hwmon0/temp2_input"), "72000\n");
            write(&device.join("hwmon/hwmon0/temp2_label"), "junction\n");
            write(&device.join("hwmon/hwmon0/power1_average"), "220000000\n");
            write(&device.join("hwmon/hwmon0/fan1_input"), "1600\n");

            #[cfg(unix)]
            {
                use std::os::unix::fs::symlink;
                symlink(&device, self.roots.pci_devices().join("0000:03:00.0"))
                    .expect("PCI device symlink");
                symlink(
                    device.join("drm/card0"),
                    self.roots.drm_class().join("card0"),
                )
                .expect("DRM card symlink");
                symlink(&device, device.join("drm/card0/device")).expect("DRM device symlink");
            }
            device
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).expect("remove fixture");
        }
    }

    fn write(path: &Path, value: &str) {
        fs::write(path, value).expect("fixture value");
    }

    #[test]
    fn unions_pci_and_drm_inventory_and_detects_amd_capabilities() {
        let fixture = Fixture::new();
        fixture.amd_gpu();
        let inventory = discover(&fixture.roots, false);
        assert_eq!(inventory.records.len(), 1);
        let record = &inventory.records[0];
        assert_eq!(record.provider_device_id, "0000:03:00.0");
        assert_eq!(record.observation.vendor, GpuVendor::Amd);
        assert_eq!(
            record.observation.memory.dedicated_total_bytes,
            Some(25_769_803_776)
        );
        assert!(
            record
                .observation
                .capabilities
                .supports(MetricKey::UtilizationOverall)
        );
        assert!(
            record
                .observation
                .capabilities
                .supports(MetricKey::TemperatureHotspotCelsius)
        );
    }

    #[test]
    fn non_display_pci_functions_are_not_gpus() {
        let fixture = Fixture::new();
        let device = fixture.roots.sys.join("devices/pci0000:00/0000:04:00.0");
        fs::create_dir_all(&device).expect("network device directory");
        write(&device.join("class"), "0x020000\n");
        write(&device.join("vendor"), "0x8086\n");
        write(&device.join("device"), "0x1234\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&device, fixture.roots.pci_devices().join("0000:04:00.0"))
            .expect("PCI device symlink");

        assert!(discover(&fixture.roots, false).records.is_empty());
    }

    #[test]
    fn intel_inventory_does_not_claim_unsupported_global_utilization() {
        let fixture = Fixture::new();
        let device = fixture.roots.sys.join("devices/pci0000:00/0000:00:02.0");
        fs::create_dir_all(&device).expect("Intel device directory");
        write(&device.join("class"), "0x030000\n");
        write(&device.join("vendor"), "0x8086\n");
        write(&device.join("device"), "0x46a6\n");
        write(&device.join("uevent"), "DRIVER=i915\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&device, fixture.roots.pci_devices().join("0000:00:02.0"))
            .expect("PCI device symlink");

        let inventory = discover(&fixture.roots, false);
        assert_eq!(inventory.records.len(), 1);
        assert_eq!(inventory.records[0].observation.vendor, GpuVendor::Intel);
        assert!(
            !inventory.records[0]
                .observation
                .capabilities
                .supports(MetricKey::UtilizationOverall)
        );
    }

    #[test]
    fn software_drm_adapters_are_filtered_unless_requested() {
        let fixture = Fixture::new();
        let device = fixture
            .roots
            .sys
            .join("devices/platform/simple-framebuffer.0");
        fs::create_dir_all(device.join("drm/card0")).expect("software DRM directory");
        write(&device.join("uevent"), "DRIVER=simpledrm\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(
                device.join("drm/card0"),
                fixture.roots.drm_class().join("card0"),
            )
            .expect("DRM card symlink");
            symlink(&device, device.join("drm/card0/device")).expect("DRM device symlink");
        }

        assert!(discover(&fixture.roots, false).records.is_empty());
        assert_eq!(discover(&fixture.roots, true).records.len(), 1);
    }
}
