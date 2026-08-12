use let_smi_core::{GpuMonitor, MonitorOptions, SampleRequest};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let monitor = GpuMonitor::open(MonitorOptions::default())?;
    println!("{}", serde_json::to_string_pretty(&monitor.diagnostics())?);
    for gpu in monitor.gpus()? {
        println!("{}", serde_json::to_string_pretty(&gpu)?);
        let snapshot = monitor.sample(&gpu.identity.id, SampleRequest::default())?;
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
    }
    monitor.close();
    Ok(())
}
