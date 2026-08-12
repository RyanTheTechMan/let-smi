# let-smi

Cross-platform GPU discovery and telemetry for Node.js. The public API is
implemented in TypeScript and backed by a prebuilt NAPI-RS native module.

```sh
npm install let-smi
```

The matching Windows, Linux, or macOS native package is installed as an
optional dependency. Normal installations do not need Rust, a compiler, or a
GPU command-line utility.

## Discovery and snapshots

```ts
import { GpuMonitor } from "let-smi";

const monitor = await GpuMonitor.open();

try {
  for (const gpu of await monitor.gpus()) {
    const snapshot = await gpu.sample();
    console.log(gpu.identity.name, snapshot.utilization.overall);
  }
} finally {
  await monitor.close();
}
```

Missing sensors and optional driver libraries are represented as unavailable
metrics or provider diagnostics. They do not prevent inventory from working.

Every metric distinguishes a real zero from an unavailable value and records
its provider, quality, and sampling timestamp:

```ts
const metric = (await gpu.sample({ windowMs: 1000 })).utilization.overall;

if (metric.available) {
  console.log(metric.value, metric.source, metric.quality);
} else {
  console.log(metric.reason, metric.message);
}
```

`gpu.vendor` discriminates `NvidiaGpu`, `AmdGpu`, `IntelGpu`, `AppleGpu`, and
`UnknownGpu`, so vendor extension methods narrow naturally in TypeScript.

## Current provider coverage

| Platform            | Inventory                                  | `utilization.overall` and live telemetry                                                                                                   |
| ------------------- | ------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------ |
| Windows             | DXGI for NVIDIA/AMD/Intel/unknown adapters | PDH WDDM engine utilization; dynamically loaded NVML adds NVIDIA memory, sensors, clocks, fans, processes, and extensions                  |
| Linux               | PCI + DRM sysfs on x64/ARM64               | NVML for NVIDIA; AMD kernel busy/memory plus hwmon sensors; Intel i915/Xe clocks and sensors (device-wide utilization remains unavailable) |
| macOS Apple Silicon | Metal                                      | dynamically loaded IOReport active residency/power plus AppleSMC temperature                                                               |
| Intel-era macOS     | Metal best effort                          | AppleSMC temperature only when safely correlatable; IOAccelerator utilization is not enabled without hardware validation                   |

ADLX and Level Zero are diagnostic-only runtime boundaries in this release.
Missing libraries do not stop the generic providers. No runtime provider invokes
`nvidia-smi`, `amd-smi`, `intel_gpu_top`, `powermetrics`, or another executable.

## Continuous sampling

The native monitor owns counter deltas and shared polling state. An
`AbortSignal`, loop exit, thrown consumer error, or monitor shutdown cancels a
subscription and releases its native resources.

```ts
const controller = new AbortController();

for await (const snapshot of gpu.samples({
  intervalMs: 1000,
  includeProcesses: true,
  signal: controller.signal,
})) {
  console.log(snapshot.utilization.overall);
}
```

## Capabilities and diagnostics

Use `gpu.supports("temperatures.coreCelsius")` before requesting UI for an
optional metric. `await monitor.diagnostics()` reports provider load status and
field-level metric-selection candidates without exposing unrelated system
information. Always call `await monitor.close()` during shutdown; closing more
than once is safe.

Apple IOReport/SMC telemetry is best effort, enabled by default, and isolated
from Metal inventory. Pass `{ enableApplePrivateTelemetry: false }` to disable
those undocumented interfaces.

Detailed architecture, provider, semantics, dependency/license, and hardware
testing notes are in the
[repository documentation](https://github.com/ryanthetechman/let-smi/tree/main/docs).
