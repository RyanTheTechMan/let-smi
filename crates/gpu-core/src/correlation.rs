use crate::model::{
    CanonicalGpu, DeviceObservation, GpuIdentity, GpuKind, GpuVendor, MacosIdentity,
    MemoryTopology, PciIdentity, StaticMemoryInfo, WindowsIdentity,
};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug)]
struct UnionFind {
    parents: Vec<usize>,
    ranks: Vec<u8>,
    vendors: Vec<Option<GpuVendor>>,
}

impl UnionFind {
    fn new(vendors: impl IntoIterator<Item = GpuVendor>) -> Self {
        let vendors: Vec<_> = vendors
            .into_iter()
            .map(|vendor| (vendor != GpuVendor::Unknown).then_some(vendor))
            .collect();
        let size = vendors.len();
        Self {
            parents: (0..size).collect(),
            ranks: vec![0; size],
            vendors,
        }
    }

    fn find(&mut self, index: usize) -> usize {
        if self.parents[index] != index {
            self.parents[index] = self.find(self.parents[index]);
        }
        self.parents[index]
    }

    fn union(&mut self, left: usize, right: usize) {
        let left_root = self.find(left);
        let right_root = self.find(right);
        if left_root == right_root {
            return;
        }
        let merged_vendor = match (self.vendors[left_root], self.vendors[right_root]) {
            (Some(left), Some(right)) if left != right => return,
            (Some(vendor), _) | (_, Some(vendor)) => Some(vendor),
            (None, None) => None,
        };
        match self.ranks[left_root].cmp(&self.ranks[right_root]) {
            std::cmp::Ordering::Less => {
                self.parents[left_root] = right_root;
                self.vendors[right_root] = merged_vendor;
            }
            std::cmp::Ordering::Greater => {
                self.parents[right_root] = left_root;
                self.vendors[left_root] = merged_vendor;
            }
            std::cmp::Ordering::Equal => {
                self.parents[right_root] = left_root;
                self.ranks[left_root] += 1;
                self.vendors[left_root] = merged_vendor;
            }
        }
    }
}

pub fn correlate(observations: Vec<DeviceObservation>) -> Vec<CanonicalGpu> {
    if observations.is_empty() {
        return Vec::new();
    }

    let mut union_find = UnionFind::new(observations.iter().map(|value| value.vendor));
    let mut strong_keys: HashMap<String, usize> = HashMap::new();

    for (index, observation) in observations.iter().enumerate() {
        for key in correlation_keys(observation) {
            if let Some(previous) = strong_keys.get(&key).copied() {
                if vendors_compatible(observations[previous].vendor, observation.vendor) {
                    union_find.union(previous, index);
                }
            } else {
                strong_keys.insert(key, index);
            }
        }
    }

    correlate_by_last_resort_heuristic(&observations, &mut union_find);

    let mut clusters: BTreeMap<usize, Vec<&DeviceObservation>> = BTreeMap::new();
    for (index, observation) in observations.iter().enumerate() {
        let root = union_find.find(index);
        clusters.entry(root).or_default().push(observation);
    }

    let mut canonical: Vec<CanonicalGpu> = clusters
        .into_values()
        .map(|mut cluster| {
            cluster.sort_by(|left, right| {
                right
                    .identity_priority
                    .cmp(&left.identity_priority)
                    .then_with(|| left.provider.cmp(&right.provider))
                    .then_with(|| left.provider_device_id.cmp(&right.provider_device_id))
            });
            merge_cluster(&cluster)
        })
        .collect();

    let provider_id_map: HashMap<(String, String), String> = canonical
        .iter()
        .flat_map(|gpu| {
            gpu.provider_device_ids.iter().map(move |(provider, id)| {
                ((provider.clone(), id.clone()), gpu.identity.id.clone())
            })
        })
        .collect();

    for gpu in &mut canonical {
        if let Some(parent_ref) = observations.iter().find_map(|observation| {
            let matches_gpu = gpu
                .provider_device_ids
                .get(&observation.provider)
                .is_some_and(|id| id == &observation.provider_device_id);
            matches_gpu
                .then(|| {
                    observation
                        .parent_provider_device_id
                        .as_ref()
                        .map(|parent| (observation.provider.clone(), parent.clone()))
                })
                .flatten()
        }) {
            gpu.identity.parent_device_id = provider_id_map.get(&parent_ref).cloned();
        }
    }

    canonical.sort_by(|left, right| left.identity.id.cmp(&right.identity.id));
    canonical
}

fn correlation_keys(observation: &DeviceObservation) -> Vec<String> {
    let mut keys = Vec::new();

    if let Some(uuid) = observation.uuid.as_deref().map(normalize_identifier)
        && !uuid.is_empty()
    {
        keys.push(format!("uuid:{uuid}"));
    }
    if let Some(address) = observation
        .pci
        .as_ref()
        .and_then(|pci| pci.address.as_deref())
        .map(normalize_pci_address)
        && !address.is_empty()
    {
        keys.push(format!("pci:{address}"));
    }
    if let Some(windows) = observation.windows.as_ref() {
        if let Some(luid) = windows.luid.as_deref().map(normalize_identifier) {
            keys.push(format!("luid:{luid}"));
        }
        if let Some(pnp) = windows.pnp_device_id.as_deref().map(normalize_identifier) {
            keys.push(format!("pnp:{pnp}"));
        }
    }
    if let Some(macos) = observation.macos.as_ref() {
        if let Some(registry_id) = macos.registry_entry_id.as_deref().map(normalize_identifier) {
            keys.push(format!("ioreg:{registry_id}"));
        }
        if let Some(metal_id) = macos.metal_registry_id.as_deref().map(normalize_identifier) {
            keys.push(format!("metal:{metal_id}"));
        }
    }

    keys
}

fn correlate_by_last_resort_heuristic(
    observations: &[DeviceObservation],
    union_find: &mut UnionFind,
) {
    let mut weak_groups: HashMap<(GpuVendor, String, u32), Vec<usize>> = HashMap::new();

    for (index, observation) in observations.iter().enumerate() {
        let Some(ordinal) = observation.enumeration_ordinal else {
            continue;
        };
        weak_groups
            .entry((
                observation.vendor,
                normalize_name(&observation.name),
                ordinal,
            ))
            .or_default()
            .push(index);
    }

    for indices in weak_groups.into_values() {
        let mut providers = std::collections::HashSet::new();
        if indices.len() < 2
            || !indices
                .iter()
                .all(|index| providers.insert(&observations[*index].provider))
        {
            continue;
        }
        let first = indices[0];
        for index in indices.into_iter().skip(1) {
            union_find.union(first, index);
        }
    }
}

fn merge_cluster(cluster: &[&DeviceObservation]) -> CanonicalGpu {
    let vendor = cluster
        .iter()
        .find_map(|observation| {
            (observation.vendor != GpuVendor::Unknown).then_some(observation.vendor)
        })
        .unwrap_or(GpuVendor::Unknown);

    let name = cluster
        .iter()
        .find(|observation| !observation.name.trim().is_empty())
        .map_or_else(
            || "Unknown GPU".to_owned(),
            |observation| observation.name.clone(),
        );
    let architecture = first_some(cluster, |observation| observation.architecture.clone());
    let driver_version = first_some(cluster, |observation| observation.driver_version.clone());
    let firmware_version = first_some(cluster, |observation| observation.firmware_version.clone());
    let kind = cluster
        .iter()
        .find_map(|observation| (observation.kind != GpuKind::Unknown).then_some(observation.kind))
        .unwrap_or_default();
    let uuid = first_some(cluster, |observation| observation.uuid.clone());
    let pci = merge_pci(cluster);
    let windows = merge_windows(cluster);
    let macos = merge_macos(cluster);
    let partition = first_some(cluster, |observation| observation.partition.clone());
    let memory = merge_memory(cluster);
    let fingerprint = best_fingerprint(
        cluster,
        vendor,
        uuid.as_deref(),
        pci.as_ref(),
        windows.as_ref(),
        macos.as_ref(),
        partition.as_ref().map(|value| value.id.as_str()),
    );
    let digest = blake3::hash(fingerprint.as_bytes()).to_hex().to_string();
    let id = format!("gpu_{vendor}_{}", &digest[..20]);

    let mut capabilities = crate::model::CapabilitySet::default();
    let mut provider_device_ids = BTreeMap::new();
    let mut vendor_info = BTreeMap::new();
    for observation in cluster.iter().rev() {
        capabilities.extend(&observation.capabilities);
        provider_device_ids.insert(
            observation.provider.clone(),
            observation.provider_device_id.clone(),
        );
        vendor_info.extend(observation.vendor_info.clone());
    }
    let public_capabilities = capabilities.to_public();
    let providers = provider_device_ids.keys().cloned().collect();

    CanonicalGpu {
        identity: GpuIdentity {
            id,
            vendor,
            name,
            architecture,
            driver_version,
            firmware_version,
            kind,
            uuid,
            pci,
            windows,
            macos,
            parent_device_id: None,
            partition,
        },
        capabilities: public_capabilities,
        memory,
        providers,
        vendor_info,
        capability_set: capabilities,
        provider_device_ids,
    }
}

fn first_some<T>(
    cluster: &[&DeviceObservation],
    accessor: impl Fn(&DeviceObservation) -> Option<T>,
) -> Option<T> {
    cluster.iter().find_map(|observation| accessor(observation))
}

fn merge_pci(cluster: &[&DeviceObservation]) -> Option<PciIdentity> {
    let candidates: Vec<_> = cluster
        .iter()
        .filter_map(|observation| observation.pci.as_ref())
        .collect();
    let base = candidates.first()?;
    Some(PciIdentity {
        address: candidates.iter().find_map(|value| value.address.clone()),
        vendor_id: candidates
            .iter()
            .find_map(|value| (value.vendor_id != 0).then_some(value.vendor_id))
            .unwrap_or(base.vendor_id),
        device_id: candidates
            .iter()
            .find_map(|value| (value.device_id != 0).then_some(value.device_id))
            .unwrap_or(base.device_id),
        subsystem_vendor_id: candidates
            .iter()
            .find_map(|value| value.subsystem_vendor_id),
        subsystem_device_id: candidates
            .iter()
            .find_map(|value| value.subsystem_device_id),
    })
}

fn merge_windows(cluster: &[&DeviceObservation]) -> Option<WindowsIdentity> {
    let windows: Vec<_> = cluster
        .iter()
        .filter_map(|observation| observation.windows.as_ref())
        .collect();
    (!windows.is_empty()).then(|| WindowsIdentity {
        luid: windows.iter().find_map(|value| value.luid.clone()),
        pnp_device_id: windows.iter().find_map(|value| value.pnp_device_id.clone()),
    })
}

fn merge_macos(cluster: &[&DeviceObservation]) -> Option<MacosIdentity> {
    let macos: Vec<_> = cluster
        .iter()
        .filter_map(|observation| observation.macos.as_ref())
        .collect();
    (!macos.is_empty()).then(|| MacosIdentity {
        registry_entry_id: macos
            .iter()
            .find_map(|value| value.registry_entry_id.clone()),
        metal_registry_id: macos
            .iter()
            .find_map(|value| value.metal_registry_id.clone()),
    })
}

fn merge_memory(cluster: &[&DeviceObservation]) -> StaticMemoryInfo {
    let topology = cluster
        .iter()
        .find_map(|observation| {
            (observation.memory.topology != MemoryTopology::Unknown)
                .then_some(observation.memory.topology)
        })
        .unwrap_or_default();
    StaticMemoryInfo {
        topology,
        dedicated_total_bytes: cluster
            .iter()
            .filter_map(|observation| observation.memory.dedicated_total_bytes)
            .max(),
        shared_total_bytes: cluster
            .iter()
            .filter_map(|observation| observation.memory.shared_total_bytes)
            .max(),
        unified_total_bytes: cluster
            .iter()
            .filter_map(|observation| observation.memory.unified_total_bytes)
            .max(),
    }
}

#[allow(clippy::too_many_arguments)]
fn best_fingerprint(
    cluster: &[&DeviceObservation],
    vendor: GpuVendor,
    uuid: Option<&str>,
    pci: Option<&PciIdentity>,
    windows: Option<&WindowsIdentity>,
    macos: Option<&MacosIdentity>,
    partition: Option<&str>,
) -> String {
    let partition_suffix = partition.map_or_else(String::new, |id| {
        format!(":partition:{}", normalize_identifier(id))
    });
    if let Some(uuid) = uuid {
        return format!(
            "{vendor}:uuid:{}{partition_suffix}",
            normalize_identifier(uuid)
        );
    }
    if let Some(address) = pci.and_then(|value| value.address.as_deref()) {
        return format!(
            "{vendor}:pci:{}:{:04x}:{:04x}{partition_suffix}",
            normalize_pci_address(address),
            pci.map_or(0, |value| value.vendor_id),
            pci.map_or(0, |value| value.device_id),
        );
    }
    if let Some(luid) = windows.and_then(|value| value.luid.as_deref()) {
        return format!(
            "{vendor}:luid:{}{partition_suffix}",
            normalize_identifier(luid)
        );
    }
    if let Some(pnp) = windows.and_then(|value| value.pnp_device_id.as_deref()) {
        return format!(
            "{vendor}:pnp:{}{partition_suffix}",
            normalize_identifier(pnp)
        );
    }
    if let Some(metal) = macos.and_then(|value| value.metal_registry_id.as_deref()) {
        return format!(
            "{vendor}:metal:{}{partition_suffix}",
            normalize_identifier(metal)
        );
    }
    if let Some(registry) = macos.and_then(|value| value.registry_entry_id.as_deref()) {
        return format!(
            "{vendor}:ioreg:{}{partition_suffix}",
            normalize_identifier(registry)
        );
    }

    let fallback = cluster[0];
    format!(
        "{vendor}:fallback:{}:{}:{}{partition_suffix}",
        fallback.provider,
        normalize_identifier(&fallback.provider_device_id),
        normalize_name(&fallback.name),
    )
}

fn normalize_identifier(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect()
}

pub fn normalize_pci_address(value: &str) -> String {
    let normalized = normalize_identifier(value);
    if normalized.matches(':').count() == 1 {
        format!("0000:{normalized}")
    } else {
        normalized
    }
}

fn normalize_name(value: &str) -> String {
    value
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn vendors_compatible(left: GpuVendor, right: GpuVendor) -> bool {
    left == right || left == GpuVendor::Unknown || right == GpuVendor::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CapabilitySet, MetricKey};

    fn observation(provider: &str, device_id: &str) -> DeviceObservation {
        let mut observation =
            DeviceObservation::new(provider, device_id, GpuVendor::Amd, "Radeon RX 7900 XTX");
        observation.pci = Some(PciIdentity {
            address: Some("0000:03:00.0".into()),
            vendor_id: 0x1002,
            device_id: 0x744c,
            subsystem_vendor_id: None,
            subsystem_device_id: None,
        });
        observation
    }

    #[test]
    fn merges_the_same_pci_device_field_by_field() {
        let mut sysfs = observation("linux-sysfs", "card0");
        sysfs.capabilities = CapabilitySet::new([MetricKey::TemperatureCoreCelsius]);
        let mut vendor = observation("amd-linux", "amd-0");
        vendor.uuid = Some("GPU-abc".into());
        vendor.capabilities = CapabilitySet::new([MetricKey::UtilizationOverall]);

        let devices = correlate(vec![sysfs, vendor]);
        assert_eq!(devices.len(), 1);
        assert!(
            devices[0]
                .capability_set
                .supports(MetricKey::TemperatureCoreCelsius)
        );
        assert!(
            devices[0]
                .capability_set
                .supports(MetricKey::UtilizationOverall)
        );
    }

    #[test]
    fn stable_id_does_not_depend_on_enumeration_order() {
        let first = observation("linux-sysfs", "card0");
        let second = observation("amd-linux", "amd-0");
        let left = correlate(vec![first.clone(), second.clone()]);
        let right = correlate(vec![second, first]);
        assert_eq!(left[0].identity.id, right[0].identity.id);
    }

    #[test]
    fn identical_names_do_not_merge_without_an_explicit_ordinal() {
        let mut first = observation("provider-a", "a");
        let mut second = observation("provider-b", "b");
        first.pci = None;
        second.pci = None;

        assert_eq!(correlate(vec![first, second]).len(), 2);
    }

    #[test]
    fn unknown_observation_cannot_bridge_conflicting_vendors() {
        let mut unknown = DeviceObservation::new("generic", "u", GpuVendor::Unknown, "GPU");
        unknown.uuid = Some("shared".into());
        let mut amd = DeviceObservation::new("amd", "a", GpuVendor::Amd, "AMD GPU");
        amd.uuid = Some("shared".into());
        let mut nvidia = DeviceObservation::new("nvidia", "n", GpuVendor::Nvidia, "NVIDIA GPU");
        nvidia.uuid = Some("shared".into());

        let devices = correlate(vec![unknown, amd, nvidia]);
        assert_eq!(devices.len(), 2);
        assert!(
            devices
                .iter()
                .any(|gpu| gpu.identity.vendor == GpuVendor::Amd)
        );
        assert!(
            devices
                .iter()
                .any(|gpu| gpu.identity.vendor == GpuVendor::Nvidia)
        );
    }
}
