use super::parse::{parse_active_dpm_clock_mhz, parse_f64, parse_u64, read_text_within};
use crate::model::{MetricKey, MetricQuality, MetricValue, UnavailableReason};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(crate) enum MetricReader {
    Percent(PathBuf),
    Bytes(PathBuf),
    MilliCelsius(PathBuf),
    MicroWatts(PathBuf),
    MicroJoules(PathBuf),
    Rpm(PathBuf),
    PwmPercent {
        input: PathBuf,
        maximum: Option<PathBuf>,
    },
    HertzToMhz(PathBuf),
    ActiveDpmMhz(PathBuf),
    MaximumMhz(Vec<PathBuf>),
}

impl MetricReader {
    fn paths(&self) -> Vec<&Path> {
        match self {
            Self::Percent(path)
            | Self::Bytes(path)
            | Self::MilliCelsius(path)
            | Self::MicroWatts(path)
            | Self::MicroJoules(path)
            | Self::Rpm(path)
            | Self::HertzToMhz(path)
            | Self::ActiveDpmMhz(path) => vec![path.as_path()],
            Self::PwmPercent { input, maximum } => {
                let mut paths = vec![input.as_path()];
                if let Some(maximum) = maximum {
                    paths.push(maximum.as_path());
                }
                paths
            }
            Self::MaximumMhz(paths) => paths.iter().map(PathBuf::as_path).collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MetricSource {
    pub key: MetricKey,
    pub reader: MetricReader,
    pub quality: MetricQuality,
    pub definition: String,
    pub priority: i16,
}

impl MetricSource {
    pub fn new(
        key: MetricKey,
        reader: MetricReader,
        quality: MetricQuality,
        definition: impl Into<String>,
        priority: i16,
    ) -> Self {
        Self {
            key,
            reader,
            quality,
            definition: definition.into(),
            priority,
        }
    }

    pub fn exists(&self, device_path: &Path) -> bool {
        self.reader
            .paths()
            .into_iter()
            .any(|path| path.exists() && read_text_within(path, device_path).is_ok())
    }

    pub fn probe(&self, device_path: &Path) -> bool {
        self.read(device_path).is_ok()
    }

    pub fn read(&self, device_path: &Path) -> Result<MetricValue, MetricReadError> {
        let value = match &self.reader {
            MetricReader::Percent(path) => {
                let value = read_f64(path, device_path)?;
                if !(0.0..=100.0).contains(&value) {
                    return Err(MetricReadError::provider(
                        path,
                        format!("percentage {value} was outside 0..=100"),
                    ));
                }
                MetricValue::Number(value)
            }
            MetricReader::Bytes(path) => MetricValue::Integer(read_u64(path, device_path)?),
            MetricReader::MilliCelsius(path) => {
                let raw = read_f64(path, device_path)?;
                let value = raw / 1_000.0;
                if !(-100.0..=250.0).contains(&value) {
                    return Err(MetricReadError::provider(
                        path,
                        format!("temperature {value} C was implausible"),
                    ));
                }
                MetricValue::Number(value)
            }
            MetricReader::MicroWatts(path) => {
                let value = read_f64(path, device_path)? / 1_000_000.0;
                MetricValue::Number(nonnegative(path, value)?)
            }
            MetricReader::MicroJoules(path) => {
                let value = read_f64(path, device_path)? / 1_000_000.0;
                MetricValue::Number(nonnegative(path, value)?)
            }
            MetricReader::Rpm(path) => {
                MetricValue::Number(nonnegative(path, read_f64(path, device_path)?)?)
            }
            MetricReader::PwmPercent { input, maximum } => {
                let input_value = read_f64(input, device_path)?;
                let maximum_value = match maximum {
                    Some(path) if path.exists() => read_f64(path, device_path)?,
                    _ => 255.0,
                };
                if maximum_value <= 0.0 {
                    return Err(MetricReadError::provider(
                        maximum.as_deref().unwrap_or(input),
                        "PWM maximum was not positive",
                    ));
                }
                let percent = input_value / maximum_value * 100.0;
                if !(0.0..=100.0).contains(&percent) {
                    return Err(MetricReadError::provider(
                        input,
                        format!("PWM percentage {percent} was outside 0..=100"),
                    ));
                }
                MetricValue::Number(percent)
            }
            MetricReader::HertzToMhz(path) => {
                let value = read_f64(path, device_path)? / 1_000_000.0;
                MetricValue::Number(nonnegative(path, value)?)
            }
            MetricReader::ActiveDpmMhz(path) => {
                let text = read_text_within(path, device_path)
                    .map_err(|error| MetricReadError::from_io(path, device_path, error))?;
                let value = parse_active_dpm_clock_mhz(&text)
                    .map_err(|message| MetricReadError::provider(path, message))?;
                MetricValue::Number(value)
            }
            MetricReader::MaximumMhz(paths) => {
                let mut values = Vec::new();
                let mut errors = Vec::new();
                for path in paths {
                    match read_f64(path, device_path) {
                        Ok(value) if value >= 0.0 => values.push(value),
                        Ok(value) => errors.push(MetricReadError::provider(
                            path,
                            format!("clock {value} MHz was negative"),
                        )),
                        Err(error) => errors.push(error),
                    }
                }
                let value = values.into_iter().max_by(f64::total_cmp).ok_or_else(|| {
                    select_error(errors).unwrap_or_else(|| {
                        MetricReadError::provider(device_path, "no clock paths were present")
                    })
                })?;
                MetricValue::Number(value)
            }
        };
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MetricReadError {
    pub reason: UnavailableReason,
    pub message: String,
}

impl MetricReadError {
    fn from_io(path: &Path, device_path: &Path, error: io::Error) -> Self {
        let reason = match error.kind() {
            io::ErrorKind::PermissionDenied => UnavailableReason::PermissionDenied,
            io::ErrorKind::NotFound if !device_path.exists() => UnavailableReason::DeviceLost,
            io::ErrorKind::NotFound => UnavailableReason::TemporarilyUnavailable,
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => {
                UnavailableReason::TemporarilyUnavailable
            }
            _ => UnavailableReason::ProviderError,
        };
        Self {
            reason,
            message: format!("could not read {}: {error}", path.display()),
        }
    }

    fn provider(path: &Path, message: impl Into<String>) -> Self {
        Self {
            reason: UnavailableReason::ProviderError,
            message: format!("{}: {}", path.display(), message.into()),
        }
    }
}

pub(crate) fn select_error(
    errors: impl IntoIterator<Item = MetricReadError>,
) -> Option<MetricReadError> {
    errors
        .into_iter()
        .max_by_key(|error| reason_rank(error.reason))
}

fn reason_rank(reason: UnavailableReason) -> u8 {
    match reason {
        UnavailableReason::PermissionDenied => 7,
        UnavailableReason::DeviceLost => 6,
        UnavailableReason::DriverLibraryMissing => 5,
        UnavailableReason::FirstSample => 4,
        UnavailableReason::ProviderError => 3,
        UnavailableReason::TemporarilyUnavailable => 2,
        UnavailableReason::Unsupported => 1,
    }
}

fn read_u64(path: &Path, device_path: &Path) -> Result<u64, MetricReadError> {
    let text = read_text_within(path, device_path)
        .map_err(|error| MetricReadError::from_io(path, device_path, error))?;
    parse_u64(&text).map_err(|message| MetricReadError::provider(path, message))
}

fn read_f64(path: &Path, device_path: &Path) -> Result<f64, MetricReadError> {
    let text = read_text_within(path, device_path)
        .map_err(|error| MetricReadError::from_io(path, device_path, error))?;
    parse_f64(&text).map_err(|message| MetricReadError::provider(path, message))
}

fn nonnegative(path: &Path, value: f64) -> Result<f64, MetricReadError> {
    (value >= 0.0)
        .then_some(value)
        .ok_or_else(|| MetricReadError::provider(path, format!("value {value} was negative")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "let-smi-linux-metric-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("fixture directory");
        path
    }

    #[test]
    fn converts_hwmon_units_without_treating_zero_as_unavailable() {
        let directory = fixture_dir();
        let temperature = directory.join("temp1_input");
        let power = directory.join("power1_average");
        fs::write(&temperature, "0\n").expect("temperature fixture");
        fs::write(&power, "125000000\n").expect("power fixture");

        let temperature = MetricSource::new(
            MetricKey::TemperatureCoreCelsius,
            MetricReader::MilliCelsius(temperature),
            MetricQuality::Direct,
            "fixture",
            1,
        );
        let power = MetricSource::new(
            MetricKey::PowerDrawWatts,
            MetricReader::MicroWatts(power),
            MetricQuality::Direct,
            "fixture",
            1,
        );
        assert_eq!(temperature.read(&directory), Ok(MetricValue::Number(0.0)));
        assert_eq!(power.read(&directory), Ok(MetricValue::Number(125.0)));
        fs::remove_dir_all(directory).expect("remove fixture");
    }

    #[test]
    fn rejects_out_of_range_percentages() {
        let directory = fixture_dir();
        let busy = directory.join("gpu_busy_percent");
        fs::write(&busy, "101\n").expect("busy fixture");
        let source = MetricSource::new(
            MetricKey::UtilizationOverall,
            MetricReader::Percent(busy),
            MetricQuality::Direct,
            "fixture",
            1,
        );
        assert!(source.read(&directory).is_err());
        fs::remove_dir_all(directory).expect("remove fixture");
    }

    #[test]
    fn maps_permission_denied_without_turning_it_into_zero() {
        let error = MetricReadError::from_io(
            Path::new("/fixture/value"),
            Path::new("/fixture/device"),
            io::Error::from(io::ErrorKind::PermissionDenied),
        );
        assert_eq!(error.reason, UnavailableReason::PermissionDenied);
    }
}
