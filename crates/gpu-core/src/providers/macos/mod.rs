#[cfg(target_arch = "aarch64")]
mod ioreport;
mod metal_inventory;
mod smc;

use crate::model::UnavailableReason;
use crate::monitor::MonitorOptions;
use crate::provider::Provider;
use crate::providers::optional_runtime::UnavailableProvider;
use std::sync::Arc;

pub fn providers(options: &MonitorOptions) -> Vec<Arc<dyn Provider>> {
    let mut providers: Vec<Arc<dyn Provider>> =
        vec![Arc::new(metal_inventory::MetalInventoryProvider)];

    if options.enable_apple_private_telemetry {
        providers.push(Arc::new(smc::AppleSmcProvider::new()));

        #[cfg(target_arch = "aarch64")]
        providers.push(Arc::new(ioreport::AppleIoReportProvider::new()));

        #[cfg(not(target_arch = "aarch64"))]
        providers.push(Arc::new(UnavailableProvider::new(
            "apple-ioreport",
            UnavailableReason::Unsupported,
            "IOReport GPU telemetry is only enabled for validated Apple Silicon targets",
        )));
    } else {
        providers.push(Arc::new(UnavailableProvider::new(
            "apple-ioreport",
            UnavailableReason::Unsupported,
            "private Apple telemetry is disabled; pass enableApplePrivateTelemetry to opt in",
        )));
        providers.push(Arc::new(UnavailableProvider::new(
            "apple-smc",
            UnavailableReason::Unsupported,
            "AppleSMC GPU telemetry is disabled; pass enableApplePrivateTelemetry to opt in",
        )));
    }

    // IOAccelerator statistics and property names are undocumented and vary
    // between Intel-era drivers. Keep the provider visible in diagnostics
    // without attaching unvalidated values to a physical GPU.
    providers.push(Arc::new(UnavailableProvider::new(
        "macos-ioaccelerator",
        UnavailableReason::Unsupported,
        if cfg!(target_arch = "x86_64") {
            "legacy IOAccelerator telemetry is disabled pending hardware validation"
        } else {
            "legacy IOAccelerator telemetry applies only to Intel-era Macs"
        },
    )));

    providers
}
