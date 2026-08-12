use std::path::PathBuf;

/// Filesystem roots used by the Linux provider.
///
/// Injecting these roots keeps sysfs and procfs parsing testable without GPU
/// hardware and without mounting a synthetic filesystem over the host paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxRoots {
    pub sys: PathBuf,
    pub proc: PathBuf,
    pub dev: PathBuf,
}

impl LinuxRoots {
    pub fn new(sys: impl Into<PathBuf>, proc: impl Into<PathBuf>, dev: impl Into<PathBuf>) -> Self {
        Self {
            sys: sys.into(),
            proc: proc.into(),
            dev: dev.into(),
        }
    }

    pub fn host() -> Self {
        Self::new("/sys", "/proc", "/dev")
    }

    pub(crate) fn pci_devices(&self) -> PathBuf {
        self.sys.join("bus/pci/devices")
    }

    pub(crate) fn drm_class(&self) -> PathBuf {
        self.sys.join("class/drm")
    }
}

impl Default for LinuxRoots {
    fn default() -> Self {
        Self::host()
    }
}
