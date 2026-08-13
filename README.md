# let-smi

Cross-platform GPU discovery and telemetry for Node.js, with a TypeScript API
backed by a Rust/NAPI-RS core.

`let-smi` discovers GPUs through platform inventory APIs without invoking vendor
command-line tools. NVIDIA, Intel, and unknown-device paths are implemented on
Windows; other platform/provider rows below distinguish implementation from
hardware validation. Providers contribute identity and metrics field-by-field,
so missing driver libraries or sensors degrade to explicit unavailable values
instead of preventing the package from loading.

```sh
npm install let-smi
```

Supported prebuilt targets are Windows x64, Linux x64/ARM64 glibc, Linux
x64 musl, and macOS x64/Apple Silicon. Normal consumers do not need Rust,
CMake, Python, a compiler, or a binary download install script.

## Quick start

```ts
import { GpuMonitor } from "let-smi";

const monitor = await GpuMonitor.open();

try {
  for (const gpu of await monitor.gpus()) {
    console.log(gpu.id, gpu.vendor, gpu.identity.name);

    const snapshot = await gpu.sample({ windowMs: 1000 });
    const busy = snapshot.utilization.overall;

    if (busy.available) {
      console.log(`${busy.value.toFixed(1)}%`, busy.source, busy.definition);
    } else {
      console.log("utilization unavailable:", busy.reason);
    }

    if (gpu.vendor === "nvidia") {
      console.log(await gpu.nvidiaInfo());
    }
  }
} finally {
  await monitor.close();
}
```

`Gpu` is a discriminated union of `NvidiaGpu`, `AmdGpu`, `IntelGpu`,
`AppleGpu`, and `UnknownGpu`. Vendor classes publicly extend `GenericGpu`, while
the implementation composes them around one private monitor client.

## Support status

The table describes the current implementation, not the theoretical capability
of a vendor SDK. A check means the provider is implemented; actual fields still
depend on the installed driver, device, permissions, and sensors.

| Platform/provider   | Inventory                                | Overall utilization                                      | Other current telemetry                                                                         |
| ------------------- | ---------------------------------------- | -------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| Windows generic     | DXGI name, IDs, memory, LUID             | PDH maximum active WDDM engine                           | graphics/compute/copy/encode/decode engine groups when present                                  |
| Windows NVIDIA      | DXGI + dynamically loaded NVML           | NVML, with PDH fallback                                  | VRAM, temperature, power/energy, clocks, fan, encoder/decoder, processes, and NVIDIA extensions |
| Windows AMD         | DXGI code path; not hardware-tested here | PDH code path; not hardware-tested here                  | ADLX is unimplemented/diagnostic-only; no AMD sensor claim                                      |
| Windows Intel       | DXGI/D3DKMT; UHD 770 tested              | PDH; UHD 770 tested                                      | Level Zero Sysman is unimplemented/diagnostic-only                                              |
| Linux generic       | PCI + DRM sysfs, driver, IDs             | provider-dependent                                       | hwmon sensors where safely attributable                                                         |
| Linux NVIDIA        | sysfs + dynamically loaded NVML          | NVML                                                     | NVML metrics and extensions as above                                                            |
| Linux AMD           | PCI/DRM sysfs                            | `gpu_busy_percent`                                       | VRAM/GTT, memory busy, hwmon temperature/power/energy/fan, DPM clocks                           |
| Linux Intel i915/Xe | PCI/DRM sysfs                            | unavailable unless a future accurate provider is present | current GT clocks and attributable hwmon sensors                                                |
| macOS Apple Silicon | Metal                                    | dynamically loaded IOReport active residency             | IOReport GPU power/energy and AppleSMC temperature                                              |
| Intel-era macOS     | Metal best effort                        | unavailable in the validated release                     | AppleSMC temperature only when a single GPU can be correlated safely                            |

Unknown vendors remain discoverable when DXGI, PCI/DRM, or Metal can enumerate
them. The Intel+NVIDIA Windows x64 hybrid path is hardware-tested. Windows
ARM64 is not supported or packaged; macOS Intel, AMD Windows hardware,
partitions, and legacy drivers require broader hardware-lab validation; see
[provider details](docs/providers.md).

## Metrics are explicit

Unavailable is not zero:

```ts
type Metric<T> =
  | {
      available: true;
      value: T;
      source: string;
      quality: "direct" | "derived" | "estimated";
      sampledAt: number;
      intervalMs?: number;
      definition?: string;
    }
  | {
      available: false;
      reason:
        | "unsupported"
        | "driver-library-missing"
        | "permission-denied"
        | "device-lost"
        | "first-sample"
        | "temporarily-unavailable"
        | "provider-error";
      source?: string;
      message?: string;
    };
```

Bytes, Celsius, watts, joules, MHz, RPM, and percent are encoded into field
names. `utilization.overall` always includes a backend-specific definition and
does not pretend every vendor reports the same concept. See
[metric semantics](docs/metric-semantics.md).

`gpu.capabilities` reflects probes performed for that device and runtime.
Convenience checks use the exact metric names:

```ts
if (gpu.supports("temperatures.coreCelsius")) {
  const temperature = (await gpu.sample()).temperatures.coreCelsius;
  // A capability can still be temporarily unavailable in an individual sample.
}
```

## Continuous sampling

The native monitor owns counter baselines and shared polling. Slow consumers get
the latest snapshot rather than an unbounded backlog. Breaking the loop,
throwing, aborting, or closing the monitor cancels the native subscription.

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

Sampling intervals are actual provider intervals and appear in `intervalMs`.
The first reading from a delta counter can be `first-sample`; a positive
one-shot `windowMs` lets the native sampler establish and retry that baseline.

## Refresh and diagnostics

```ts
await monitor.refresh();

const diagnostics = await monitor.diagnostics();
for (const provider of diagnostics.providers) {
  console.log(provider.id, provider.loaded, provider.reason, provider.message);
}
```

Diagnostics include platform/architecture, provider load state, matched-device
counts, warnings, and field-level merge candidates after sampling. They do not
include environment variables, command output, or unrelated system data.

Optional providers normally never make `open()` throw. For controlled deployments
you can require one explicitly:

```ts
await GpuMonitor.open({ requiredProviders: ["nvml"] });
```

Apple IOReport and AppleSMC telemetry use undocumented protocols, are enabled by
default, dynamically/defensively accessed, and never block Metal inventory. Set
`enableApplePrivateTelemetry: false` to disable both.

## Vendor extensions

Vendor methods return this project's stable schema, not upstream NVML/ADLX/
Level Zero types:

```ts
if (gpu.vendor === "nvidia") {
  const info = await gpu.nvidiaInfo();
  console.log(info.cudaComputeCapability, info.mig, info.pState);
} else if (gpu.vendor === "amd") {
  console.log(await gpu.amdInfo());
} else if (gpu.vendor === "intel") {
  console.log(await gpu.intelInfo());
} else if (gpu.vendor === "apple") {
  console.log(await gpu.appleInfo());
}
```

The vendor schemas are future-compatible and mostly optional. Fields absent from
the installed provider/device are omitted rather than guessed.

## Packaging and development

The public package declares exact-version optional packages for each native
target. A custom loader selects the local/prebuilt `.node` file and detects Linux
libc through `process.report`; it does not execute `ldd` or download a binary.
GPU driver libraries are never bundled.

Development requires Node.js 20+, pnpm, and Rust 1.88+:

```sh
pnpm install --frozen-lockfile --ignore-scripts
pnpm check
pnpm native:build:release
pnpm native:test-loader
pnpm pack:check
pnpm test:linux-hardware # skips unless NVIDIA + Intel are both present
```

Repository documentation:

- [architecture](docs/architecture.md)
- [providers and support details](docs/providers.md)
- [metric semantics](docs/metric-semantics.md)
- [security and reliability](docs/security.md)
- [dependency and licensing audit](docs/dependencies.md)
- [testing and hardware matrix](docs/testing.md)

## Known limitations

- ADLX and Level Zero are safe runtime diagnostic boundaries, not telemetry
  implementations yet.
- Windows ARM64 is not a supported or packaged target.
- Accurate Intel Linux device-wide utilization needs Level Zero Sysman or an
  i915 PMU implementation; per-client DRM fdinfo is not presented as overall.
- Intel-era IOAccelerator telemetry is disabled pending real-hardware validation.
- Apple IOReport/SMC are private interfaces and may become unavailable after an
  OS update; Metal inventory continues independently.
- Process telemetry is conservative. Current NVML support reports process
  framebuffer usage but does not fabricate per-process utilization.
- A stable ID can change if an OS exposes only a session-scoped identifier or a
  newly installed provider supplies a stronger identity than before.

Licensed under MIT. See [the dependency audit](docs/dependencies.md) before
adding a provider SDK or native dependency.
