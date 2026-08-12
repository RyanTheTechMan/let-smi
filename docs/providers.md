# Providers and support details

Providers return partial observations. The monitor correlates them into physical
GPUs and selects telemetry per field; there is no single “provider for a GPU.”
Capabilities are probed for the installed runtime/device and are not inferred
from vendor name alone.

## Provider summary

| Provider ID           | Platform            | Inventory                                                                                                          | Telemetry                                                  | Loading behavior                                                |
| --------------------- | ------------------- | ------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------- | --------------------------------------------------------------- |
| `windows-dxgi`        | Windows x64         | adapter name, vendor/device/subsystem IDs, LUID, PCI location, dedicated/shared totals, physical-adapter filtering | none                                                       | Windows DXGI and D3DKMT APIs                                    |
| `windows-pdh`         | Windows x64         | contributes no duplicate inventory                                                                                 | WDDM engine and overall utilization                        | persistent bounded Windows PDH query; first reading unavailable |
| `nvml`                | Windows/Linux       | NVIDIA UUID, name, PCI, driver, architecture, VBIOS, VRAM                                                          | NVIDIA utilization, memory, sensors, processes, extensions | dynamically loads host NVML; never requires `nvidia-smi`        |
| `linux-sysfs`         | Linux x64/ARM64     | PCI display devices unioned with DRM card/render nodes, driver, memory, partitions                                 | AMD sysfs plus attributable hwmon; Intel clocks/hwmon      | Rust filesystem access only; no `libdrm` hard link              |
| `macos-metal`         | macOS x64/ARM64     | Metal name, registry ID, location/kind, GPU families, unified-memory topology                                      | none                                                       | public OS framework                                             |
| `apple-ioreport`      | Apple Silicon macOS | one Apple GPU observation used for correlation                                                                     | active residency and GPU energy/power                      | private framework loaded by absolute OS path and symbol-checked |
| `apple-smc`           | macOS x64/ARM64     | Metal-correlated sensor observation                                                                                | best-effort GPU die temperature                            | public IOKit calls to an undocumented AppleSMC protocol         |
| `amd-adlx`            | Windows x64         | none                                                                                                               | unimplemented                                              | secure system-DLL presence diagnostic only                      |
| `level-zero`          | Windows x64         | none                                                                                                               | unimplemented                                              | secure system-DLL presence diagnostic only                      |
| `macos-ioaccelerator` | Intel-era macOS     | none                                                                                                               | none in the validated release                              | explicit unsupported diagnostic boundary                        |

## Correlation and provider priority

Identity providers use strong cross-provider keys whenever available. Current
identity priorities are NVML (100), DXGI/Metal (80), Linux sysfs (60), IOReport
(60), and SMC (10). Higher identity priority supplies descriptive fields but
does not own the entire GPU.

Telemetry priority is per metric. The important current rules are:

| Device/field                                          | Preferred source                       | Fallback                                                                                          |
| ----------------------------------------------------- | -------------------------------------- | ------------------------------------------------------------------------------------------------- |
| NVIDIA utilization/sensors/memory on Windows or Linux | NVML                                   | Windows PDH for utilization only; Linux unavailable without NVML except attributable hwmon fields |
| AMD Windows utilization                               | PDH                                    | unavailable                                                                                       |
| Intel Windows utilization                             | PDH                                    | unavailable                                                                                       |
| AMD Linux overall/memory busy/VRAM/GTT                | AMD kernel sysfs through `linux-sysfs` | unavailable                                                                                       |
| Linux temperature/power/fan                           | attributable hwmon source              | unavailable                                                                                       |
| Intel Linux clocks                                    | i915/Xe sysfs                          | unavailable                                                                                       |
| Apple Silicon utilization/power                       | IOReport                               | unavailable                                                                                       |
| macOS GPU temperature                                 | AppleSMC                               | unavailable                                                                                       |

NVML has the highest priorities for its metrics. IOReport has the highest Apple
utilization/power priorities, SMC has a temperature-specific priority, and
generic providers score below vendor-specific direct providers. Quality and
freshness remain part of the score. `diagnostics().metricSelections` records
candidate scores and the selected source after sampling.

## Windows

### DXGI inventory

DXGI is the baseline and works without a vendor SDK. It enumerates
`IDXGIAdapter1`, filters `DXGI_ADAPTER_FLAG_SOFTWARE` unless
`includeSoftwareAdapters` is true, and records the adapter LUID. D3DKMT enriches
each LUID with PCI bus/device/function and hybrid integrated/discrete flags.
Hybrid presentation paths carrying invalid D3DKMT PCI sentinels are excluded as
non-physical duplicates. Dedicated and shared values come from
`DXGI_ADAPTER_DESC1` and are static capacities; the provider deliberately does
not misrepresent per-process DXGI budget APIs as system-wide used VRAM.

Kind is `virtual` for software, `integrated`/`discrete` when D3DKMT reports a
hybrid role, and otherwise `unknown`. DXCore and PNP enrichment are not
implemented. PCI identity is preferred for cross-provider correlation and LUID
remains the strongest Windows-specific key.

### PDH GPU Engine

One persistent English PDH query reads
`\\GPU Engine(*)\\Utilization Percentage`. It correlates counter instances to a
DXGI adapter by LUID, sums processes that reference the same physical engine,
caps counter rounding noise at 100%, and takes the maximum engine as overall.
The query is shared by the monitor. One formatted-array collection is reused
for adapters sampled in the same 10 ms batch so a hybrid sample has a common,
meaningful interval. The first counter collection marks every PDH rate field
`first-sample`; no zero is invented.

The provider emits graphics, compute, copy, encoder, and decoder fields only
when matching WDDM engine types appear in the counter data. PDH process IDs are
not yet normalized into process snapshots. Formatted arrays are limited to 16
MiB and 65,536 items, use checked size arithmetic, validate the x64 ABI and
embedded UTF-16 pointers, and allow at most three `PDH_MORE_DATA` retries.

### Optional Windows vendor runtimes

NVML is fully implemented and dynamically loaded on Windows only from absolute
paths derived from `GetSystemDirectoryW` and
`SHGetKnownFolderPath(FOLDERID_ProgramFiles)`. Each candidate is first loaded
with `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32`; the same
absolute path is then passed to `nvml-wrapper`. Current directory, `PATH`,
environment-selected driver paths, and process-global `SetDllDirectoryW` are
not used.

ADLX and Level Zero are secure
`LoadLibraryExW(..., LOAD_LIBRARY_SEARCH_SYSTEM32)` presence probes only.
Diagnostics always report them as nonfunctional: `unsupported` when the DLL is
detected and `driver-library-missing` otherwise. They advertise no metric
capabilities and never fabricate AMD/Intel sensor data. No AMD hardware or ADLX
telemetry was validated. Level Zero Sysman is not implemented.

Windows ARM64 is not a supported or packaged target in this release. The
Windows provider behavior described here is x64-only.

## Linux

`linux-sysfs` joins `/sys/bus/pci/devices` display-class functions with primary
`/sys/class/drm` card/render nodes through canonical device paths. It also
supports non-PCI platform devices where DRM exposes them. `simpledrm`,
`simple-framebuffer`, `efi-framebuffer`, `vkms`, and `vgem` are filtered by
default.

The provider caches immutable discovery records and probes each metric file for
the actual device. Read failures become `permission-denied`, `device-lost`,
`temporarily-unavailable`, or `provider-error`. Sysfs values are size-bounded and
strictly parsed.

### AMD Linux

Current AMD kernel interfaces:

- `gpu_busy_percent` and `mem_busy_percent`;
- `mem_info_vram_total/used` and `mem_info_gtt_total/used`;
- active `pp_dpm_sclk` and `pp_dpm_mclk` states;
- labelled or documented-index hwmon edge, hotspot/junction, and memory
  temperatures;
- hwmon average/input power, power cap, cumulative energy, fan RPM/PWM, and
  labelled clocks.

The release does not ship `libamdgpu_top` or AMD SMI. This keeps Linux gnu/musl
and non-AMD installations free from optional `libdrm`/ROCm load-time
dependencies. Process accounting, compute-unit data, performance levels, and
advanced XGMI/firmware data remain future work.

### Intel Linux

Both `i915` and `xe` inventory are supported. The provider reads current GT
frequency paths that exist on the installed kernel and attributable hwmon
sensors. It intentionally does not claim device-wide overall utilization:
standard DRM fdinfo is per-client and permission-limited, and upstream Intel
engine sysfs paths do not provide a portable device busy counter. A future
dynamically loaded Level Zero Sysman or i915 PMU backend should supply this.

### NVIDIA Linux

Sysfs provides generic PCI/DRM identity and NVML supplies NVIDIA-specific
identity/telemetry. `libnvidia-ml.so.1` is loaded at runtime through
`nvml-wrapper`; its absence leaves sysfs discovery operational.

## NVIDIA NVML

NVML capabilities are probed call-by-call for each device. Depending on driver
and hardware, the provider exposes:

- UUID, PCI identity, architecture, driver/VBIOS, and framebuffer total;
- GPU and memory-controller utilization, encoder, and decoder utilization;
- framebuffer used bytes, temperature, power draw/limit, total energy, clocks,
  fan percent/RPM;
- compute and graphics process framebuffer allocations;
- CUDA compute capability, SM count, P-state, throttle reasons, ECC mode, MIG
  mode, BAR1, PCIe generation/width, compute mode, encoder sessions, and thermal
  thresholds.

Device reacquisition prefers UUID, then PCI bus ID, then the original index only
as a last resort. A failure in one NVML field does not discard other fields.
MIG partition enumeration, NVLink, vGPU, and ECC counter details remain future
work and need the documented hardware matrix.

## macOS

### Metal inventory

Metal is always the baseline. It exposes device name, registry ID, removable/
external/low-power hints, supported Metal families, and unified-memory topology.
On unified-memory systems the total is host physical memory. Metal's
`currentAllocatedSize` and recommended working-set size are not exposed as
system-wide GPU memory used; the latter remains vendor information only.

### Apple IOReport

IOReport is private and Apple Silicon-only. The provider:

- loads only absolute OS-owned framework paths;
- resolves every required symbol before use;
- version-gates unvalidated macOS major versions;
- subscribes only to GPU performance-state residency and GPU energy channels;
- owns CoreFoundation objects with deterministic release;
- returns `first-sample` until it has two samples.

Failure never disables Metal inventory. Set
`enableApplePrivateTelemetry: false` to disable IOReport and SMC. The current
maximum validated major is macOS 27; later versions return an unavailable
diagnostic until tested.

### AppleSMC

The SMC provider opens the `AppleSMC` IOKit user client once, performs bounded
key discovery, and accepts only exact `Tg*` little-endian `flt ` or `TG*`
big-endian signed `sp78` temperature encodings. One sensor is direct; multiple
GPU die sensors are averaged and marked estimated. Chassis fan keys are never
presented as GPU fan metrics.

On multi-GPU Intel-era Macs a single undifferentiated SMC sensor group is
ambiguous, so it is left unmatched. Legacy IOAccelerator statistics remain
diagnostic-only because their private properties could not be safely validated
without representative Intel/AMD Mac hardware.

## Capability and failure rules

- A provider advertises a metric only after its library, device, and field probe
  succeeds.
- A capability can still fail temporarily in an individual sample; the metric
  then contains an unavailable reason and source.
- Missing optional libraries do not throw during normal `open()`.
- `requiredProviders` is the explicit opt-in exception and rejects an unknown or
  unloaded required provider.
- Hardware reset, removal, permission change, and topology refresh never turn
  into numeric zero.

See [metric semantics](metric-semantics.md) for exact definitions and
[testing](testing.md) for the hardware matrix.
