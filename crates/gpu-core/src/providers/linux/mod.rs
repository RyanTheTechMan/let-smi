mod hwmon;
mod inventory;
mod metric;
mod parse;
mod provider;
mod roots;

pub(crate) use provider::LinuxSysfsProvider;
pub(crate) use roots::LinuxRoots;

use crate::monitor::MonitorOptions;
use crate::provider::Provider;
use std::sync::Arc;

pub fn providers(options: &MonitorOptions) -> Vec<Arc<dyn Provider>> {
    vec![Arc::new(LinuxSysfsProvider::new(
        LinuxRoots::host(),
        options.include_software_adapters,
    ))]
}
