pub mod mock;

#[cfg(any(target_os = "linux", windows, test))]
pub mod level_zero;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

pub mod nvml;
pub mod optional_runtime;

use crate::monitor::MonitorOptions;
use crate::provider::Provider;
use std::sync::Arc;

pub fn default_providers(options: &MonitorOptions) -> Vec<Arc<dyn Provider>> {
    let mut providers: Vec<Arc<dyn Provider>> = Vec::new();

    #[cfg(target_os = "linux")]
    {
        providers.extend(linux::providers(options));
    }
    #[cfg(target_os = "macos")]
    {
        providers.extend(macos::providers(options));
    }
    #[cfg(windows)]
    {
        providers.extend(windows::providers(options));
    }
    #[cfg(any(target_os = "linux", windows))]
    {
        providers.push(Arc::new(nvml::NvmlProvider::new()));
        providers.push(Arc::new(level_zero::LevelZeroProvider::new()));
    }

    providers
}
