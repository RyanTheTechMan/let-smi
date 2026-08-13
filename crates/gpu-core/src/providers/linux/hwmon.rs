use super::metric::{MetricReader, MetricSource};
use super::parse::{numbered_attribute, read_text_within};
use crate::model::{GpuVendor, MetricKey, MetricQuality};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_HWMON_DIRECTORIES: usize = 32;
const MAX_HWMON_DIRECTORY_ENTRIES: usize = 128;

pub(crate) fn discover(device_path: &Path, vendor: GpuVendor) -> Vec<MetricSource> {
    let hwmon_root = device_path.join("hwmon");
    let mut directories = read_directories(&hwmon_root, device_path);
    directories.sort();

    let mut sources = Vec::new();
    for directory in directories {
        let hwmon_name = read_text_within(&directory.join("name"), device_path).unwrap_or_default();
        discover_temperatures(&directory, device_path, vendor, &hwmon_name, &mut sources);
        discover_power(&directory, device_path, &hwmon_name, &mut sources);
        discover_fans(&directory, device_path, &hwmon_name, &mut sources);
        discover_clocks(&directory, device_path, &hwmon_name, &mut sources);
    }
    sources
}

fn discover_temperatures(
    directory: &Path,
    device_path: &Path,
    vendor: GpuVendor,
    hwmon_name: &str,
    sources: &mut Vec<MetricSource>,
) {
    for (index, input) in numbered_files(directory, "temp", "_input") {
        let label =
            read_text_within(&directory.join(format!("temp{index}_label")), device_path).ok();
        let Some((key, quality, semantic)) = temperature_semantic(vendor, index, label.as_deref())
        else {
            continue;
        };
        let sensor = label.as_deref().unwrap_or(semantic);
        sources.push(MetricSource::new(
            key,
            MetricReader::MilliCelsius(input),
            quality,
            format!(
                "Linux hwmon {sensor} temperature in Celsius{}",
                provider_suffix(hwmon_name)
            ),
            if label.is_some() { 120 } else { 90 },
        ));
    }
}

fn temperature_semantic(
    vendor: GpuVendor,
    index: u32,
    label: Option<&str>,
) -> Option<(MetricKey, MetricQuality, &'static str)> {
    if let Some(label) = label {
        let label = normalize_label(label);
        if contains_any(&label, &["junction", "hotspot", "hot spot"]) {
            return Some((
                MetricKey::TemperatureHotspotCelsius,
                MetricQuality::Direct,
                "hotspot",
            ));
        }
        if contains_any(&label, &["memory", "mem", "vram", "hbm"]) {
            return Some((
                MetricKey::TemperatureMemoryCelsius,
                MetricQuality::Direct,
                "memory",
            ));
        }
        if label.contains("edge") {
            return Some((
                MetricKey::TemperatureEdgeCelsius,
                MetricQuality::Direct,
                "edge",
            ));
        }
        if contains_any(&label, &["gpu", "core", "package", "soc", "graphics"]) {
            return Some((
                MetricKey::TemperatureCoreCelsius,
                MetricQuality::Direct,
                "core",
            ));
        }
        return None;
    }

    if vendor == GpuVendor::Amd {
        return match index {
            1 => Some((
                MetricKey::TemperatureEdgeCelsius,
                MetricQuality::Direct,
                "documented AMD edge sensor",
            )),
            2 => Some((
                MetricKey::TemperatureHotspotCelsius,
                MetricQuality::Direct,
                "documented AMD junction sensor",
            )),
            3 => Some((
                MetricKey::TemperatureMemoryCelsius,
                MetricQuality::Direct,
                "documented AMD memory sensor",
            )),
            _ => None,
        };
    }

    (index == 1).then_some((
        MetricKey::TemperatureCoreCelsius,
        MetricQuality::Estimated,
        "unlabelled primary GPU sensor",
    ))
}

fn discover_power(
    directory: &Path,
    device_path: &Path,
    hwmon_name: &str,
    sources: &mut Vec<MetricSource>,
) {
    for (index, path) in numbered_files(directory, "power", "_average") {
        let label = sensor_label(directory, device_path, "power", index);
        sources.push(MetricSource::new(
            MetricKey::PowerDrawWatts,
            MetricReader::MicroWatts(path),
            MetricQuality::Direct,
            format!(
                "Linux hwmon average GPU power in watts{}{}",
                label_suffix(label.as_deref()),
                provider_suffix(hwmon_name)
            ),
            130,
        ));
    }
    for (index, path) in numbered_files(directory, "power", "_input") {
        let label = sensor_label(directory, device_path, "power", index);
        sources.push(MetricSource::new(
            MetricKey::PowerDrawWatts,
            MetricReader::MicroWatts(path),
            MetricQuality::Direct,
            format!(
                "Linux hwmon instantaneous GPU power in watts{}{}",
                label_suffix(label.as_deref()),
                provider_suffix(hwmon_name)
            ),
            120,
        ));
    }
    for (index, path) in numbered_files(directory, "power", "_cap") {
        let label = sensor_label(directory, device_path, "power", index);
        sources.push(MetricSource::new(
            MetricKey::PowerLimitWatts,
            MetricReader::MicroWatts(path),
            MetricQuality::Direct,
            format!(
                "Linux hwmon GPU power cap in watts{}{}",
                label_suffix(label.as_deref()),
                provider_suffix(hwmon_name)
            ),
            120,
        ));
    }
    for (index, path) in numbered_files(directory, "energy", "_input") {
        let label = sensor_label(directory, device_path, "energy", index);
        sources.push(MetricSource::new(
            MetricKey::PowerEnergyJoules,
            MetricReader::MicroJoules(path),
            MetricQuality::Direct,
            format!(
                "Linux hwmon cumulative GPU energy in joules{}{}",
                label_suffix(label.as_deref()),
                provider_suffix(hwmon_name)
            ),
            120,
        ));
    }
}

fn discover_fans(
    directory: &Path,
    device_path: &Path,
    hwmon_name: &str,
    sources: &mut Vec<MetricSource>,
) {
    for (index, path) in numbered_files(directory, "fan", "_input") {
        let label = sensor_label(directory, device_path, "fan", index);
        sources.push(MetricSource::new(
            MetricKey::FanRpm,
            MetricReader::Rpm(path),
            MetricQuality::Direct,
            format!(
                "Linux hwmon GPU fan speed in RPM{}{}",
                label_suffix(label.as_deref()),
                provider_suffix(hwmon_name)
            ),
            120,
        ));
    }

    for (index, input) in numbered_files(directory, "pwm", "") {
        let maximum = directory.join(format!("pwm{index}_max"));
        let maximum = maximum.exists().then_some(maximum);
        sources.push(MetricSource::new(
            MetricKey::FanPercent,
            MetricReader::PwmPercent { input, maximum },
            MetricQuality::Derived,
            format!(
                "Linux hwmon GPU PWM duty cycle as a percentage{}",
                provider_suffix(hwmon_name)
            ),
            110,
        ));
    }
}

fn discover_clocks(
    directory: &Path,
    device_path: &Path,
    hwmon_name: &str,
    sources: &mut Vec<MetricSource>,
) {
    for (index, path) in numbered_files(directory, "freq", "_input") {
        let Ok(label) =
            read_text_within(&directory.join(format!("freq{index}_label")), device_path)
        else {
            continue;
        };
        let normalized = normalize_label(&label);
        let key = if contains_any(&normalized, &["memory", "mem", "mclk"]) {
            MetricKey::ClockMemoryMhz
        } else if contains_any(&normalized, &["graphics", "gfx", "sclk", "core", "gpu"]) {
            MetricKey::ClockGraphicsMhz
        } else if contains_any(&normalized, &["video", "media", "vclk"]) {
            MetricKey::ClockVideoMhz
        } else {
            continue;
        };
        sources.push(MetricSource::new(
            key,
            MetricReader::HertzToMhz(path),
            MetricQuality::Direct,
            format!(
                "Linux hwmon {label} clock in MHz{}",
                provider_suffix(hwmon_name)
            ),
            100,
        ));
    }
}

fn read_directories(path: &Path, device_path: &Path) -> Vec<PathBuf> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .take(MAX_HWMON_DIRECTORIES)
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|kind| kind.is_dir() || kind.is_symlink())
                .and_then(|_| fs::canonicalize(entry.path()).ok())
                .filter(|path| path.starts_with(device_path))
                .filter(|path| path.is_dir())
        })
        .collect()
}

fn numbered_files(directory: &Path, prefix: &str, suffix: &str) -> Vec<(u32, PathBuf)> {
    let mut files: Vec<_> = fs::read_dir(directory)
        .into_iter()
        .flatten()
        .take(MAX_HWMON_DIRECTORY_ENTRIES)
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            let index = numbered_attribute(name, prefix, suffix)?;
            entry
                .file_type()
                .ok()
                .filter(|kind| kind.is_file() || kind.is_symlink())
                .map(|_| (index, entry.path()))
        })
        .collect();
    files.sort_by_key(|(index, path)| (*index, path.clone()));
    files
}

fn sensor_label(directory: &Path, device_path: &Path, prefix: &str, index: u32) -> Option<String> {
    read_text_within(
        &directory.join(format!("{prefix}{index}_label")),
        device_path,
    )
    .ok()
}

fn normalize_label(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(['_', '-'], " ")
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn provider_suffix(hwmon_name: &str) -> String {
    if hwmon_name.is_empty() {
        String::new()
    } else {
        format!(" ({hwmon_name})")
    }
}

fn label_suffix(label: Option<&str>) -> String {
    label.map_or_else(String::new, |label| format!(" ({label})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn amd_unlabelled_temperature_indices_use_documented_meanings() {
        assert_eq!(
            temperature_semantic(GpuVendor::Amd, 1, None).map(|value| value.0),
            Some(MetricKey::TemperatureEdgeCelsius)
        );
        assert_eq!(
            temperature_semantic(GpuVendor::Amd, 2, None).map(|value| value.0),
            Some(MetricKey::TemperatureHotspotCelsius)
        );
        assert_eq!(
            temperature_semantic(GpuVendor::Amd, 3, None).map(|value| value.0),
            Some(MetricKey::TemperatureMemoryCelsius)
        );
    }

    #[test]
    fn labelled_temperature_wins_over_vendor_index_fallback() {
        assert_eq!(
            temperature_semantic(GpuVendor::Amd, 1, Some("junction")).map(|value| value.0),
            Some(MetricKey::TemperatureHotspotCelsius)
        );
    }

    #[test]
    fn limits_sensor_attributes_per_hwmon_directory() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "let-smi-linux-hwmon-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("hwmon fixture directory");
        for index in 1..=MAX_HWMON_DIRECTORY_ENTRIES + 1 {
            fs::write(directory.join(format!("temp{index}_input")), "42000\n")
                .expect("sensor fixture");
        }

        assert_eq!(
            numbered_files(&directory, "temp", "_input").len(),
            MAX_HWMON_DIRECTORY_ENTRIES
        );
        fs::remove_dir_all(directory).expect("remove hwmon fixture");
    }

    #[test]
    fn limits_attributable_hwmon_directories() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let device = std::env::temp_dir().join(format!(
            "let-smi-linux-hwmon-directories-{}-{nonce}",
            std::process::id()
        ));
        let hwmon = device.join("hwmon");
        for index in 0..=MAX_HWMON_DIRECTORIES {
            fs::create_dir_all(hwmon.join(format!("hwmon{index}")))
                .expect("hwmon directory fixture");
        }

        assert_eq!(
            read_directories(&hwmon, &device).len(),
            MAX_HWMON_DIRECTORIES
        );
        fs::remove_dir_all(device).expect("remove hwmon fixture");
    }
}
