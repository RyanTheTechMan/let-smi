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

| Platform            | Inventory                                                          | `utilization.overall` and live telemetry                                                                                                        |
| ------------------- | ------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| Windows x64         | DXGI/D3DKMT for NVIDIA/Intel/unknown; AMD generic path is untested | PDH WDDM engines; optional Intel Sysman engines/sensors/clocks/power; NVML adds NVIDIA memory, sensors, clocks, fans, processes, and extensions |
| Linux               | PCI + DRM sysfs on x64/ARM64                                       | NVML for NVIDIA; AMD kernel busy/memory plus hwmon; Intel i915/Xe clocks and sensors plus optional Level Zero Sysman engine occupancy           |
| macOS Apple Silicon | Metal                                                              | dynamically loaded IOReport active residency/power plus AppleSMC temperature                                                                    |
| Intel-era macOS     | Metal best effort                                                  | AppleSMC temperature only when safely correlatable; IOAccelerator utilization is not enabled without hardware validation                        |

ADLX remains an unimplemented, diagnostic-only runtime boundary. Intel Level Zero
Sysman telemetry is implemented on Windows and Linux. Windows UHD 770 engine
occupancy and GPU frequency were validated for 0.2.0, with an unresolved Windows
watchdog outlier. Linux Intel/Sysman validation remains pending: the Bazzite 0.2.0
host exposed only an RTX 4060 Ti and lacked Level Zero runtimes, so Intel-required
runs failed and hybrid runs skipped. Separate NVIDIA NVML and sampling lifecycle
checks passed five consecutive runs. Windows ARM64 is not a supported package target.
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

## Sampling every GPU

`sampleAll()` returns a `GpuMonitorSnapshot` with a batch timestamp and an ordered
`gpus` array of `{ deviceId, snapshot }` entries. `samplesAll()` streams the same
shape and shares native scalar polling with per-GPU streams, including when
consumers request different process settings. Empty inventory produces `gpus: []`.
Each batch uses current inventory, so future batches reflect `monitor.refresh()`.
Per-metric timestamps and intervals remain authoritative.

```ts
const batch = await monitor.sampleAll({ windowMs: 1000 });
for (const { deviceId, snapshot } of batch.gpus) {
  console.log(deviceId, snapshot.utilization.overall);
}

const controller = new AbortController();
for await (const batch of monitor.samplesAll({
  intervalMs: 1000,
  signal: controller.signal,
})) {
  console.log(batch.sampledAt, batch.gpus);
}
```

One-shot warmup waits asynchronously on the native sampler schedule, allowing
other streams and refresh requests to continue.

## Capabilities and diagnostics

Use `gpu.supports("temperatures.coreCelsius")` before requesting UI for an
optional metric. `await monitor.diagnostics()` reports provider load status and
field-level metric-selection candidates without exposing unrelated system
information. Always call `await monitor.close()` during shutdown; closing more
than once is safe. Pending stream reads wait asynchronously without consuming a
libuv worker, and explicit close has a bounded exceptional path.

Apple IOReport/SMC telemetry is best effort, enabled by default, and isolated
from Metal inventory. Pass `{ enableApplePrivateTelemetry: false }` to disable
those undocumented interfaces.

Detailed architecture, provider, semantics, dependency/license, and hardware
testing notes are in the
[repository documentation](https://github.com/ryanthetechman/let-smi/tree/main/docs).
