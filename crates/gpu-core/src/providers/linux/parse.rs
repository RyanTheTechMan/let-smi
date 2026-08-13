use std::fs;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

const MAX_SYSFS_VALUE_BYTES: u64 = 64 * 1024;

pub(crate) fn read_text(path: &Path) -> io::Result<String> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_SYSFS_VALUE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SYSFS_VALUE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sysfs value exceeded the provider size limit",
        ));
    }
    let value = String::from_utf8(bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("sysfs value was not UTF-8: {error}"),
        )
    })?;
    Ok(value.trim_matches(['\0', '\n', '\r', ' ', '\t']).to_owned())
}

/// Reads a sysfs attribute only after resolving it beneath a trusted canonical
/// device directory. This keeps test-root injection from accidentally turning
/// a fixture symlink into an arbitrary host-file read.
pub(crate) fn read_text_within(path: &Path, canonical_root: &Path) -> io::Result<String> {
    let canonical = fs::canonicalize(path)?;
    if !canonical.starts_with(canonical_root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "sysfs attribute resolved outside its trusted device directory",
        ));
    }
    read_text(&canonical)
}

pub(crate) fn parse_u64(value: &str) -> Result<u64, String> {
    let value = value.trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).map_err(|error| error.to_string())
    } else {
        value.parse::<u64>().map_err(|error| error.to_string())
    }
}

pub(crate) fn parse_f64(value: &str) -> Result<f64, String> {
    let parsed = value
        .trim()
        .parse::<f64>()
        .map_err(|error| error.to_string())?;
    parsed
        .is_finite()
        .then_some(parsed)
        .ok_or_else(|| "value was not finite".to_owned())
}

pub(crate) fn parse_active_dpm_clock_mhz(value: &str) -> Result<f64, String> {
    let active_lines: Vec<_> = value.lines().filter(|line| line.contains('*')).collect();
    if active_lines.is_empty() {
        return Err("no active DPM state was marked".into());
    }

    active_lines
        .into_iter()
        .filter_map(clock_before_mhz)
        .max_by(f64::total_cmp)
        .ok_or_else(|| "active DPM state did not contain an MHz value".into())
}

fn clock_before_mhz(line: &str) -> Option<f64> {
    let lowercase = line.to_ascii_lowercase();
    let mhz_offset = lowercase.find("mhz")?;
    let before_unit = &line[..mhz_offset];
    let number_reversed: String = before_unit
        .chars()
        .rev()
        .skip_while(|character| character.is_ascii_whitespace())
        .take_while(|character| character.is_ascii_digit() || *character == '.')
        .collect();
    if number_reversed.is_empty() {
        return None;
    }
    number_reversed
        .chars()
        .rev()
        .collect::<String>()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

pub(crate) fn drm_node_name(name: &str) -> bool {
    name.strip_prefix("card")
        .or_else(|| name.strip_prefix("renderD"))
        .is_some_and(|suffix| {
            !suffix.is_empty() && suffix.chars().all(|character| character.is_ascii_digit())
        })
}

pub(crate) fn numbered_attribute(name: &str, prefix: &str, suffix: &str) -> Option<u32> {
    let number = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
    (!number.is_empty() && number.chars().all(|character| character.is_ascii_digit()))
        .then(|| number.parse().ok())
        .flatten()
}

pub(crate) fn uevent_value<'a>(contents: &'a str, key: &str) -> Option<&'a str> {
    contents.lines().find_map(|line| {
        let (candidate, value) = line.split_once('=')?;
        (candidate == key).then_some(value.trim())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture_path(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "let-smi-linux-parse-{}-{name}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn parses_hex_and_decimal_sysfs_numbers() {
        assert_eq!(parse_u64("0x1002\n"), Ok(0x1002));
        assert_eq!(parse_u64("4294967296"), Ok(4_294_967_296));
        assert!(parse_u64("18446744073709551616").is_err());
    }

    #[test]
    fn parses_active_amd_dpm_clock_without_assuming_state_index() {
        let value = "0: 500Mhz\n1: 2100Mhz *\n2: 2400Mhz\n";
        assert_eq!(parse_active_dpm_clock_mhz(value), Ok(2_100.0));
        assert!(parse_active_dpm_clock_mhz("0: 500Mhz").is_err());
    }

    #[test]
    fn accepts_only_primary_drm_node_names() {
        assert!(drm_node_name("card0"));
        assert!(drm_node_name("renderD128"));
        assert!(!drm_node_name("card0-DP-1"));
        assert!(!drm_node_name("card"));
    }

    #[test]
    fn parses_numbered_hwmon_attributes_strictly() {
        assert_eq!(
            numbered_attribute("temp12_input", "temp", "_input"),
            Some(12)
        );
        assert_eq!(
            numbered_attribute("temperature_input", "temp", "_input"),
            None
        );
    }

    #[test]
    fn rejects_oversized_and_non_utf8_sysfs_values() {
        let oversized = fixture_path("oversized");
        fs::write(&oversized, vec![b'x'; MAX_SYSFS_VALUE_BYTES as usize + 1])
            .expect("oversized fixture");
        assert_eq!(
            read_text(&oversized).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        fs::remove_file(&oversized).expect("remove oversized fixture");

        let non_utf8 = fixture_path("non-utf8");
        fs::write(&non_utf8, [0xff]).expect("non-UTF-8 fixture");
        assert_eq!(
            read_text(&non_utf8).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        fs::remove_file(&non_utf8).expect("remove non-UTF-8 fixture");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_attributes_outside_the_trusted_device_directory() {
        use std::os::unix::fs::symlink;

        let root = fixture_path("root");
        let device = root.join("device");
        let outside = fixture_path("outside");
        fs::create_dir_all(&device).expect("device fixture");
        fs::write(&outside, "42\n").expect("outside fixture");
        symlink(&outside, device.join("value")).expect("hostile fixture symlink");

        assert_eq!(
            read_text_within(&device.join("value"), &device)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );

        fs::remove_file(&outside).expect("remove outside fixture");
        fs::remove_dir_all(&root).expect("remove root fixture");
    }
}
