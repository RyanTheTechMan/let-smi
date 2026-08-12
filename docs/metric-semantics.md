# Metric semantics

The public snapshot uses common field names but retains provider, quality,
timestamp, interval, and definition. Consumers must not compare values from two
backends as if their underlying counters were identical.

## Availability

Every live field is a `Metric<T>`:

- `available: true` contains a real value, including zero;
- `available: false` contains a reason and optional source/message.

Reasons mean:

| Reason                    | Meaning                                                           |
| ------------------------- | ----------------------------------------------------------------- |
| `unsupported`             | no loaded provider/device interface implements this field         |
| `driver-library-missing`  | an optional vendor/runtime library is absent                      |
| `permission-denied`       | the interface exists but the current process cannot read it       |
| `device-lost`             | a previously correlated device disappeared or reset               |
| `first-sample`            | a delta/rate counter has no previous baseline yet                 |
| `temporarily-unavailable` | the field is supported but this sample produced no usable reading |
| `provider-error`          | a provider returned an invalid value or an unexpected API error   |

Unsupported optional snapshot fields are normally omitted. The required
`utilization.overall` field is always present and contains `unsupported` when no
provider supports it. A capability is a runtime support indication, not a
promise that every future sample succeeds.

## Quality

| Quality     | Meaning                                                                                          |
| ----------- | ------------------------------------------------------------------------------------------------ |
| `direct`    | the provider reports this semantic and unit directly; it does not imply cross-vendor equivalence |
| `derived`   | the library calculates the value from counters, energy, residency, or several direct readings    |
| `estimated` | attribution or aggregation is useful but not an exact device-defined metric                      |

## `utilization.overall`

All ordinary utilization fields are percentages in 0–100. The exact current
backend definition is also copied into `metric.definition`.

| Source               | Definition                                                                                          | Quality and interval                                                                                                                       |
| -------------------- | --------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `nvml`               | Percentage of NVML's internal sample period during which one or more kernels executed on the GPU    | `direct`; NVML does not expose the overall sample duration through this call, so `intervalMs` is absent                                    |
| `windows-pdh`        | Maximum active WDDM engine percentage across the correlated adapter                                 | `derived`; processes are summed per physical engine, then the maximum engine is selected; `intervalMs` is the real PDH collection interval |
| `linux-sysfs` on AMD | Value of the kernel driver's `gpu_busy_percent`, described as SMU-reported GPU busy percentage      | `direct`; point reading with no library-controlled interval                                                                                |
| `apple-ioreport`     | Non-idle GPU performance-state residency divided by total GPU state residency over the sample delta | `derived`; `intervalMs` is the monotonic delta interval                                                                                    |
| Intel Linux          | No accurate device-wide provider in this release                                                    | unavailable; per-client DRM fdinfo is not silently promoted to overall                                                                     |
| Intel-era macOS      | No validated live provider in this release                                                          | unavailable; private IOAccelerator values are not guessed                                                                                  |

The merge engine selects one of these definitions; it never averages or adds
incompatible overall values. On Windows NVIDIA, NVML normally wins over PDH.
AMD/Intel Windows use PDH until a vendor-specific provider is implemented.

### Engine utilization

- NVML `memoryController` is the percentage of its internal sample period during
  which device memory was read or written.
- NVML encoder/decoder values use the sampling period returned by NVML.
- PDH graphics/compute/copy/encoder/decoder values are the maximum matching WDDM
  engine percentage after per-process instances for the same physical engine
  are summed.
- AMD `bandwidthUtilizationPercent` maps to `mem_busy_percent` and is not the
  same semantic as bytes-per-second bandwidth saturation on every vendor.

## Memory

All memory quantities are bytes.

Static total fields (`dedicatedTotalBytes`, `sharedTotalBytes`, and
`unifiedTotalBytes`) come from correlated inventory and are plain optional
numbers. Live used/budget fields are metrics with provenance.

- DXGI dedicated/shared totals are adapter capacities from
  `DXGI_ADAPTER_DESC1`.
- NVML dedicated total/used is NVIDIA framebuffer memory.
- AMD Linux dedicated fields are VRAM; shared fields are GTT. GTT is an aperture/
  driver memory concept and should not be interpreted as all host RAM used by
  the GPU.
- Apple unified total is host physical memory on a unified-memory system. No
  public, system-wide Metal GPU-used value is claimed, so `unifiedUsedBytes`
  remains unavailable.

`topology` can be `dedicated`, `shared`, `unified`, `mixed`, or `unknown`. A
zero-byte used value is available zero, not missing data. Used values are
validated as non-negative; they are not clamped to a total because budgets,
driver accounting domains, and samples can differ.

## Temperature

Temperature field names encode Celsius. Negative Celsius readings are valid at
the canonical layer; each hardware provider may apply a documented plausibility
range to reject corrupt sensor encodings.

- NVML's GPU sensor maps to `coreCelsius`.
- AMD labelled `edge`, `junction`/`hotspot`, and memory sensors map to the
  corresponding fields. For unlabelled AMD hwmon, documented indices 1/2/3 are
  edge/junction/memory.
- An unlabelled primary non-AMD hwmon sensor is `coreCelsius` with `estimated`
  quality.
- AppleSMC exact GPU keys map to `coreCelsius`; one sensor is direct and an
  arithmetic mean of multiple die sensors is estimated.

No provider substitutes CPU/package temperature for GPU temperature without
explicit GPU attribution.

## Power and energy

Power is watts and energy is joules.

- NVML power is the driver's device power reading; its limit is the currently
  enforced limit. NVML energy is cumulative since the last driver reload.
- Linux hwmon micro-watts/micro-joules are converted by exactly 1,000,000.
  Energy inputs are cumulative if the kernel attribute is cumulative.
- Apple IOReport energy is the interval delta. `drawWatts` is that delta divided
  by the monotonic interval. A zero energy delta produces available zero power,
  not unavailable.

Because `energyJoules` can be cumulative or interval-local, consumers must read
`definition` and `intervalMs` before integrating or differencing it.

## Clocks and fans

Clocks are MHz. NVML clock domains map directly to graphics, compute/SM, memory,
and video. AMD active DPM states and Intel i915/Xe frequency attributes are
strictly parsed; multiple Intel GT/tile readings use the maximum current clock
and describe that aggregation.

Fan speed is either RPM or percent. NVML multi-fan values are arithmetic means
with the aggregation in `definition`. Linux PWM is converted to percent using
`pwmN_max` or 255 when the standard maximum attribute is absent. Apple chassis
fans are not exposed as GPU fans.

## Processes

Process entries are conservative. Current NVML support merges compute and
graphics process lists by PID and exposes framebuffer allocation as
`memoryUsedBytes`. A process name is included only when the operating system/
NVML call succeeds. A supported successful query with no processes returns
`processes: []`; unsupported or failed process telemetry omits `processes`
instead of presenting unavailability as an empty result.

No current backend fabricates process utilization. PDH instance PIDs and DRM
fdinfo require additional lifetime, namespace, and attribution work before they
can become reliable cross-platform process snapshots.

## Timestamps, intervals, and staleness

`sampledAt` is Unix epoch milliseconds recorded when the provider produced the
observation. `intervalMs`, when present, is the actual measured monotonic
interval—not merely the requested watch interval.

Counter providers return `first-sample` for every affected rate field until a
baseline exists. A one-shot positive `windowMs` lets the native worker wait and
retry when any requested snapshot field has that state. Continuous listeners
share provider baselines, so their requested interval controls delivery cadence
while each metric's measured `intervalMs` remains authoritative. On Windows,
PDH collections for adapters in the same sampling batch are coalesced and share
the same interval.

Candidates more than 30 seconds old cannot win field selection. They remain in
merge diagnostics with `selected: false`; if every candidate is stale, the
field is `temporarily-unavailable`.

## Field selection

Selection is deterministic and per metric. A score combines explicit metric
priority, provider specificity, provider reliability, quality, and freshness.
Invalid numeric observations cannot win. Ties are resolved by provider ID for
stable diagnostics.

Use `monitor.diagnostics().metricSelections` after a sample to see every valid
candidate, its score, and the winner. This makes fallback behavior observable
without leaking upstream provider types into the public API.
