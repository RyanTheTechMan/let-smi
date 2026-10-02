mod dxgi;
pub(crate) mod nvml_loader;
mod pdh;
mod runtime_probe;

use crate::monitor::MonitorOptions;
use crate::provider::Provider;
use std::sync::Arc;

pub fn providers(options: &MonitorOptions) -> Vec<Arc<dyn Provider>> {
    vec![
        Arc::new(dxgi::DxgiProvider::new(options.include_software_adapters)),
        Arc::new(pdh::PdhProvider::new()),
        Arc::new(runtime_probe::WindowsRuntimeProbe::system32(
            "amd-adlx",
            "amdadlx64.dll",
            "ADLX runtime detected; the SDK-governed telemetry adapter requires separate legal review",
        )),
    ]
}
