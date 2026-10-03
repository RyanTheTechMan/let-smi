# Testing and hardware validation

The repository separates deterministic tests from hardware-dependent smoke
tests. A missing GPU, optional runtime, sensor, or permission must cause a skip
or an unavailable result—not a test failure or fabricated value.

## Local quality gates

From the repository root:

```sh
pnpm install --frozen-lockfile --ignore-scripts
pnpm format:check
pnpm lint
pnpm typecheck
pnpm test
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
pnpm subprocess:check
pnpm native:config:check
pnpm native:build:release
pnpm native:validate-artifact aarch64-apple-darwin # select the host target
pnpm build
pnpm native:test-loader
pnpm pack:check
pnpm pack:test-local
pnpm test:linux-hardware
```

`native:test-loader` opens the monitor through the published ESM entry point,
checks discovery/diagnostics, calls `close()` twice, and proves the process exits.
It does not bypass the custom package loader.

Before a release candidate is pushed, also run the advisory and workflow
security checks:

```sh
pnpm audit --audit-level low
cargo audit
zizmor --pedantic .
node scripts/validate-packaging.mjs
node scripts/check-no-subprocess.mjs
```

`cargo audit` and `zizmor` are maintainer tools rather than repository runtime
dependencies. A RustSec maintenance warning is not equivalent to a security
advisory; record both separately. `validate-packaging.mjs` enforces commit-pinned
Actions, a digest-pinned container, exact native target/package sets, and the
absence of an environment-controlled addon path.

## Deterministic Rust coverage

Core tests use mock providers and injected Linux filesystem roots. They cover:

- strong-key correlation and order-independent stable IDs;
- prevention of name-only duplicate merging;
- field-level priorities and merge diagnostics;
- stale/invalid observation rejection;
- explicit unsupported, first-sample, and zero-value behavior;
- unit conversion and bounds for bytes, Celsius, watts, joules, MHz, RPM, and
  percentages;
- Linux PCI/DRM union discovery, AMD sysfs/hwmon fixtures, device loss, and
  permission/error mapping;
- bounded PCI/DRM, hwmon, sensor, tile, and GT enumeration, oversized and
  malformed attributes, integer overflow, and hostile symlink containment;
- NVML conversions and unavailable-platform behavior without an NVIDIA GPU;
- IOReport state/energy calculations and AppleSMC key decoding without relying
  on a particular sensor value;
- batch/per-GPU scalar coalescing with mixed process settings, nonblocking
  warmup, empty inventory, Rust metric-filter compatibility, cancellation
  wake-up, monitor-close wake-up, and
  idempotent provider shutdown;
- bounded sampler commands/subscriptions, one in-flight `next()`, nonblocking
  finalization, and close-aware queued calls;
- injected Intel Sysman enumeration, partial support, counter resets,
  device-wide aggregate preference, maximum-group fallback, GPU power attribution,
  and sensor permission failures independent of other fields;
- trusted Windows NVML candidates and malformed/oversized/truncated PDH arrays.

Windows x64 is the only packaged Windows target in this release. Linux
x64/ARM64 and macOS x64/ARM64 remain in the native build matrix; Linux musl is
built separately. Windows ARM64 is neither claimed nor packaged.

## Deterministic TypeScript coverage

Vitest uses a fake native binding to cover:

- construction and narrowing of every vendor subclass;
- cached discovery and refresh invalidation;
- native payload validation, duplicate IDs, and invalid metrics;
- preservation of available zero versus unavailable values;
- sample/watch option validation and process options;
- vendor extension access;
- nested native merge diagnostics flattened into the public shape;
- stream completion, early break, thrown/aborted consumers, and cancellation;
- idempotent close, use-after-close, and pending-next wake-up behavior;
- custom loader selection and actionable missing-package errors.

`tests/type-narrowing.ts` is compiled by `tsc` and ensures the `Gpu` union narrows
to `NvidiaGpu`, `AmdGpu`, `IntelGpu`, `AppleGpu`, or `UnknownGpu` through
`gpu.vendor`.

## Golden invariants

Automated tests and runtime validators enforce these invariants:

- unsupported is never represented as numeric zero;
- ordinary utilization and fan percentages remain in 0–100;
- memory, energy, power, clocks, and fan speed are non-negative;
- memory is bytes, temperature Celsius, power watts, energy joules, and clocks
  MHz;
- stable IDs do not depend on enumeration order;
- strong identifiers merge multiple provider observations into one GPU;
- optional vendor runtimes cannot prevent monitor initialization;
- runtime sources contain no child-process API or telemetry executable;
- cancellation and `close()` release native resources and allow Node to exit.

## Hardware integration matrix

Every row should validate discovery, stable IDs across repeated enumeration,
capabilities, a bounded sample, clean shutdown, and diagnostic behavior. Metrics
unsupported by the hardware/driver are acceptable unavailable values.

| Platform            | Required lab coverage                                                                                                          |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| Windows x64         | NVIDIA-only; AMD-only; Intel integrated; Intel+NVIDIA; Intel+AMD; multiple NVIDIA; multiple AMD                                |
| Linux               | NVIDIA; AMD; Intel i915; Intel Xe; hybrid; headless; x64 glibc; ARM64 glibc; x64 musl load test                                |
| macOS               | Apple M1, M2, M3, M4-or-newer; Intel integrated; Intel Mac with AMD discrete where available                                   |
| Virtual/partitioned | VM/no GPU; disabled device; NVIDIA MIG; vGPU; SR-IOV; eGPU attach/detach                                                       |
| Failure modes       | missing vendor libraries; permission-denied sysfs; reset/device lost; sleep/wake; driver reload; private Apple API unavailable |

Hardware tests must identify their prerequisites and skip cleanly. They must not
install driver libraries, ask for sudo/admin, or infer success from a nonzero
sensor value.

## Manual sample checklist

For a new hardware/driver combination:

1. Capture `monitor.diagnostics()` before sampling.
2. Call `gpus()` twice and confirm IDs and device count are stable.
3. Request a 1-second sample and verify every available metric's unit, range,
   timestamp, source, quality, and definition.
4. Run two subscriptions at different delivery intervals and stop both early.
5. Call `refresh()`, then sample again.
6. Close twice and confirm the Node process exits without a forced timeout.
7. Repeat with the optional vendor runtime hidden or absent.

Diagnostic output is appropriate for bug reports because it contains provider
status and merge choices, but no environment variables, command output, file
contents, or unrelated system inventory.

## Observed Windows x64 validation — 0.1.0, 2026-08-12

Hardware-tested on Windows 10 Pro display version 25H2, build 26200.8973, x64,
as a normal user. The host had one Intel UHD Graphics 770 and one NVIDIA
GeForce RTX 5090. Node was 25.9.0, pnpm 10.32.1, Rust/Cargo 1.88.0, NVIDIA
driver 610.47, and NVML 13.610.47.

Implemented and hardware-tested:

- DXGI/D3DKMT returned exactly two physical adapters, with correct vendor,
  device/subsystem, PCI, LUID, hybrid kind, and dedicated/shared memory fields;
- repeated enumeration, refresh, concurrent samples, monitor reopen, and a
  worker thread preserved device count and stable IDs;
- NVML loaded from the Windows system directory, correlated to the DXGI NVIDIA
  adapter by PCI identity, and did not create a duplicate;
- RTX 5090 exposed NVML overall and memory-controller utilization, framebuffer
  total/used, temperature, power/limit/energy, four clock domains, fan percent
  and RPM, encoder/decoder utilization, requested processes, and NVIDIA vendor
  information. PDH supplied uncovered graphics/copy engine fields;
- Intel UHD 770 used DXGI identity/memory and PDH overall/graphics/copy/decoder
  utilization. A real idle `0` was available; compute/encoder were unavailable
  when no matching counters appeared. No Intel temperature, power, clocks, or
  process telemetry was claimed;
- the first Intel stream sample returned `first-sample` for every PDH rate
  field; the next stream sample exposed its measured interval, while the
  one-shot validation sample used a measured 1,000 ms interval;
- four pending 60-second subscription reads left
  `fs.promises.readFile()` completing in 1–2 ms; AbortSignal, early break,
  monitor close with pending `next()`, two shared listeners, and worker-thread
  isolation all passed. Normal `close()` took 2–3 ms, and the child process
  exited within its 20-second hard deadline.

Secure diagnostic probes only: ADLX was absent; the Level Zero loader DLL was
detected, but Sysman telemetry is unimplemented and the provider remained
`loaded: false`/`unsupported` with zero matches. Unimplemented or untested here:
AMD Windows telemetry/hardware, ADLX telemetry, Level Zero Sysman, Windows
ARM64, multiple physical NVIDIA GPUs, partitions, vGPU/MIG, device
reset/removal, sleep/wake, and permission-denied driver configurations.

Run the repeatable hybrid test after a debug or release native build:

```sh
pnpm test:windows-hardware
```

It skips cleanly unless Windows x64 has exactly one Intel and one NVIDIA
physical adapter with functional PDH and NVML. The parent process enforces the
exit deadline.

## Observed Linux x64 validation — 0.1.0, 2026-08-12

Hardware-tested as a normal user on Bazzite 44, kernel
7.1.5-ogc5.1.fc44.x86_64, x86_64 glibc 2.43. Tooling was Node 26.7.0, pnpm
11.19.0, and Rust/Cargo 1.95.0. This host exposed one NVIDIA GeForce RTX 4060 Ti
and **did not expose an Intel GPU through PCI/DRM sysfs**, so it did not match the
requested Intel+NVIDIA hybrid lab configuration.

Hardware-tested on the NVIDIA device:

- Linux sysfs merged `card1` and `renderD128` with NVML into one canonical GPU
  at normalized PCI address `0000:01:00.0`; repeated enumeration, refresh,
  monitor reopen, concurrent sampling, and a worker thread preserved its stable
  ID, with no duplicate DRM or NVML device;
- the NVIDIA 610.43.03 driver loaded NVML 13.610.43.03 dynamically. A sample
  exposed NVML overall and memory-controller utilization, framebuffer total and
  used bytes, temperature, power draw/limit/cumulative energy, graphics/SM/
  memory/video clocks, fan percent/RPM, encoder/decoder utilization, processes,
  and NVIDIA vendor information. An idle decoder reading of `0` remained an
  available NVML metric;
- field-selection diagnostics selected NVML for NVIDIA overall utilization and
  reported no warnings. Sysfs contributed identity/DRM data but this host did
  not expose an equivalent attributable NVIDIA hwmon candidate;
- four pending 60-second streams left `fs.promises.readFile()` completing in
  1 ms. AbortSignal cancellation, early break, pending-read monitor close,
  idempotent close, use-after-close rejection, and worker isolation passed;
  closing four pending streams took 5 ms.

Unavailable on this machine: Intel discovery, i915/Xe correlation and clocks,
Intel attributable sensors, and Intel device-wide utilization. Device-wide
Intel utilization remains intentionally unimplemented rather than derived from
per-process DRM fdinfo. The repeatable hybrid test therefore skipped with the
exact reason `requires at least one Intel GPU`:

```sh
pnpm test:linux-hardware
```

The script runs the full hybrid discovery, provenance, stream, shutdown,
process-option, refresh/reopen, and worker-thread assertions when both an NVIDIA
GPU with NVML and an Intel i915/Xe GPU are present. Linux x64 glibc is
hardware/load-tested here; Linux ARM64 glibc and x64 musl remain compile/load
coverage in CI, not hardware claims. AMD, multiple NVIDIA GPUs, MIG/vGPU,
reset, sleep/wake, hot-unplug, and permission-denied real hardware were not
exercised.

## Observed Apple Silicon validation — 2026-08-12

Hardware-tested on an Apple M5 Max running macOS 27 as a normal user. Metal
discovered one stable Apple GPU. The host exposed 11,884 total IOReport channels
before filtering, including the supported GPU residency and energy channels;
the provider retained only its bounded GPU subset. A measured sample returned
derived utilization, power, and energy from IOReport plus an estimated
temperature from 64 AppleSMC GPU die sensors. Public ESM and CommonJS loading,
the arm64 Mach-O artifact check, the complete Rust/NAPI test suite, and strict
Clippy all passed without `sudo` or an external executable.

## Registry publication checks

Registry smoke tests wait for the root package and every platform-specific
optional package to become installable before testing ESM, CommonJS, and native
loading. This accounts for npm's independent publish-time scan of each package;
an accepted publication can remain unavailable for several minutes while that
scan completes.

The `Verify published release` GitHub workflow can rerun this registry matrix
for an already-published version without rebuilding or republishing it.

## 0.2.0 Intel and monitor-wide release validation

Version 0.2.0 is prepared for release with the documented limitations below:
Windows Intel Sysman and Windows/Linux NVIDIA have hardware evidence; Linux Intel
Sysman/hybrid operation remains unvalidated, and the original Windows watchdog
outlier remains unexplained. These are accepted release limitations, not passing
hardware checks. Intel-required tests still fail when Intel hardware or usable
Sysman readings are absent; hybrid tests retain their explicit prerequisite skips.
The earlier validation records below preserve their original findings and caveats.

After building the native addon and public package on each Intel host, run:

```sh
pnpm check
pnpm native:build:release
pnpm build
pnpm native:test-loader
pnpm test:intel-hardware --require-intel-telemetry
```

The Intel test requires Windows or Linux and validates batch inventory/IDs,
provenance and bounds, four pending streams, process options, abort and early
break, worker isolation, refresh/reopen, and clean shutdown. Its parent enforces
a 40-second exit deadline. The required mode fails when no Sysman field is
available; Linux also requires measured Intel utilization. Without that flag,
non-Intel hosts skip cleanly and missing telemetry is reported without being
mistaken for hardware validation.

On an Intel/NVIDIA hybrid host, additionally run the existing
`pnpm test:windows-hardware` or `pnpm test:linux-hardware`. Save the JSON report
from the Intel test, which records device and driver identity, capabilities,
observed fields, measured intervals, and diagnostics. Hardware validation for the
new Intel provider passed on the Windows host below; Linux Intel validation remains
**pending**. The Windows watchdog outlier below remains a reliability caveat.

### Observed Windows Intel/NVIDIA validation — 0.2.0, 2026-10-02

Started from `4532e9f9626a92933a4766b1e67f4e3b3f758f2f` after fetching origin,
on `codex/windows-intel-validation`. The starting checkout was clean. No release
was published.

Validation ran without an elevated administrator token. Host and active runtimes:

| Component            | Observed version/identity                                                                                        |
| -------------------- | ---------------------------------------------------------------------------------------------------------------- |
| OS                   | Windows 11 Pro, display version 26H2, build 26300.9550, x64                                                      |
| CPU                  | Intel Core i9-12900K                                                                                             |
| Intel GPU            | UHD Graphics 770, integrated, PCI `0000:00:02.0`, device `8086:4680`, subsystem `1458:d000`                      |
| Intel display driver | `32.0.101.6129` (signed, `oem157.inf`)                                                                           |
| Level Zero loader    | System32 `ze_loader.dll` file/product version `1.17.42`                                                          |
| Active Intel runtime | DriverStore `ze_intel_gpu64.dll` file/product version `23.20.101.6129`                                           |
| NVIDIA GPU           | GeForce RTX 4070 Ti, discrete, PCI `0000:01:00.0`, device `10de:2782`, subsystem `1458:40cb`, Ada Lovelace       |
| NVIDIA driver / NVML | `617.14` / `13.617.14`; signed Windows driver `32.0.16.1714` (`oem361.inf`)                                      |
| NVML DLL             | Loaded through System32; file/product version `8.17.16.1714`; the mapped DriverStore module had the same version |
| Tooling              | Node `25.9.0`, installed pnpm `11.19.0`, Rust/Cargo `1.88.0`                                                     |

Loaded-module inspection confirmed the active Intel runtime, rather than assuming
that every installed DriverStore copy was in use. The Level Zero tracing layer
also mapped from System32 at `1.17.42`; its debug traces went to stderr. Four
virtual display adapters visible to Windows did not add physical GPU duplicates.
DXGI/PDH matched two devices, NVML matched one, and Sysman matched one Intel PCI
identity. ADLX was explicitly `driver-library-missing`; diagnostics had no warnings.

Observed sources and intervals in the saved Intel JSON report:

| Reading                                    | Source / quality            | Observation                                                                                                                                                                   |
| ------------------------------------------ | --------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Intel overall utilization                  | `level-zero` / derived      | `0.958517%`, device-wide all-engine active residency, measured `1009 ms`                                                                                                      |
| Intel compute utilization                  | `level-zero` / derived      | `0.692247%`, maximum measured compute-group occupancy, measured `1009 ms`                                                                                                     |
| Intel graphics clock                       | `level-zero` / direct       | Actual GPU-domain frequency `1550 MHz`; no synthetic interval                                                                                                                 |
| Intel engine extension                     | `level-zero` / derived      | `all`, `compute`, and combined `media` groups; media's measured idle `0` remained available; no encode/decode split was guessed                                               |
| Intel graphics/copy/decoder                | `windows-pdh` / derived     | Graphics `1.723751%`; copy and decoder available `0`; measured `1007 ms`                                                                                                      |
| Intel encoder                              | `windows-pdh` / unavailable | `temporarily-unavailable`: no matching WDDM encode counter                                                                                                                    |
| Intel sensors and other unsupported fields | Capability false / omitted  | GPU/memory temperature, GPU power/energy, memory clock, fan, and process telemetry unsupported; no zero was fabricated                                                        |
| NVIDIA telemetry                           | `nvml` / direct             | Overall/memory-controller utilization, VRAM used, temperature, power/limit/energy, graphics/SM/memory/video clocks, fan percent/RPM, encoder/decoder, and requested processes |
| NVIDIA encoder/decoder                     | `nvml` / direct             | Available idle `0`, NVML-reported `100 ms` intervals                                                                                                                          |
| NVIDIA graphics/copy fallback              | `windows-pdh` / derived     | Available idle `0`, measured `1007 ms`; NVIDIA compute counter explicitly unavailable                                                                                         |

The NVIDIA sample recorded 141,918,208 used VRAM bytes, 51 °C, 39.161 W,
285 W enforced limit, 29,763.793 J cumulative energy, graphics/SM clocks
2640 MHz, memory 10501 MHz, video 2115 MHz, and two process entries. Under WDDM
those process entries lacked per-process memory/utilization readings; these were
omitted. Idle fan `0%`/`0 RPM` remained available in the Intel report; a later
hybrid report also observed NVML fan `66%`/`1499 RPM`.

Batch and per-GPU streams ran concurrently at requested delivery intervals of
200 ms and 500 ms. Their saved Intel counter intervals were 218 ms and 93 ms
(PDH 218 ms and 95 ms), respectively: shared faster polling advances counter
baselines, so measured intervals need not equal delivery intervals. A separate
fresh 250 ms Intel stream returned `first-sample`, then a `level-zero` reading
with a measured 265 ms interval. The hybrid one-shot sample measured Sysman
1017 ms and PDH 1015 ms.

All requested commands passed in their final runs:

| Check                                                  | Result                                                                                                                                                             |
| ------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `pnpm install --frozen-lockfile --ignore-scripts`      | Passed with `CI=true` after the initial noninteractive module-purge prompt; lockfile unchanged                                                                     |
| `pnpm check`                                           | Passed: formatting, lint, typecheck, 20 TypeScript tests, 70 core + 2 NAPI Rust tests, strict Clippy, six-target packaging configuration, and subprocess scan      |
| `pnpm native:build:release`                            | Passed, Windows x64 release addon                                                                                                                                  |
| `pnpm build`                                           | Passed, ESM/CommonJS and declarations                                                                                                                              |
| `pnpm native:validate-artifact x86_64-pc-windows-msvc` | Passed, 2,693,632-byte x64 PE addon                                                                                                                                |
| `pnpm native:test-loader`                              | Passed, ESM/CommonJS public loader and idempotent close                                                                                                            |
| `pnpm pack:test-local`                                 | Passed, `let-smi-0.2.0.tgz` installed locally; actionable missing-addon error; ESM/CommonJS each discovered two GPUs, sampled batches, aborted streams, and exited |
| `pnpm test:intel-hardware --require-intel-telemetry`   | Passed with actual Sysman telemetry; no skip; unchanged 40-second parent deadline                                                                                  |
| `pnpm test:windows-hardware`                           | Passed; no skip; unchanged 20-second parent deadline                                                                                                               |

Hardware assertions covered stable IDs through repeated enumeration, refresh,
batch delivery, monitor reopen, and worker isolation. The IDs were
`gpu_intel_b1709f985028a46d84f8` and `gpu_nvidia_c70bfd049153a30d7724`. Four pending
60-second batch streams alternated process settings: both process-enabled streams
received two NVIDIA processes, both disabled streams omitted them, and Intel
processes stayed absent in every stream. A concurrent filesystem read took 1 ms;
abort of all four reads rounded to 0 ms; closing with a pending batch read took
6 ms. Early break, different delivery intervals, idempotent close, use-after-close
rejection, and worker exit passed. A worker collected Intel/NVIDIA telemetry and
closed; the main monitor retained its available Sysman/NVML fields afterward.
Deterministic Rust coverage separately proved scalar polling coalesces across
batch/per-device consumers with mixed process settings.

The initial hybrid failure was an obsolete assertion requiring PDH to win Intel
overall utilization. It now requires Sysman to win when an available Sysman
candidate exists, requires its higher score, and retains PDH as the visible
fallback. Otherwise PDH must win. NVIDIA NVML priority/fallback assertions remain
in force. Intel assertions now check process omission/enrichment, worker
telemetry survival, measured intervals, metric bounds, and cancellation latency.
The hybrid worker test now explicitly waits for a successful worker exit and
compares its IDs to the main monitor's IDs. No runtime loading policy was changed:
Sysman retains its System32-only search; NVML retains validated OS-derived
absolute paths and restricted dependency loading.

One subsequent hybrid run exceeded its 20-second watchdog without a captured
phase. Investigation could not reproduce it in 12 instrumented child runs
(1897–1948 ms). The watchdog now preserves child stderr and the test emits phase
markers to stderr. After explicitly awaiting worker exit, the final parent test
passed five consecutive runs (1945–1978 ms), plus its ordinary pnpm invocation.
The original timeout's cause is **unresolved**, so it remains a reliability caveat;
the deadline was not increased and no retry-to-pass behavior was added to the
tests. Intel Sysman metric validation is confirmed on this host, but release
readiness still needs Linux Intel validation and resolution or explanation of
that watchdog outlier. Real device loss/reset, sleep/wake, missing Intel runtime,
and Intel sensor-permission failures were not hardware-tested here; existing
deterministic coverage is not a hardware claim.

Local evidence is saved in the ignored `artifacts/windows-intel-validation/`
directory: `intel-report.json`, `windows-report.json`, `host.json`, command logs,
and repeat-run summaries. The Intel JSON was saved by invoking Node directly,
with stderr separated, and parsed successfully as JSON:

```powershell
node scripts/test-intel-hardware.mjs --require-intel-telemetry > artifacts/windows-intel-validation/intel-report.json 2> artifacts/windows-intel-validation/intel-report.stderr.log
```

**Linux Intel/Sysman hardware validation remains pending.** Neither the historical
Linux NVIDIA-only 0.1.0 run nor the Bazzite 0.2.0 run below satisfies it.

### Observed Bazzite NVIDIA-only validation — 0.2.0, 2026-10-02

Fetched origin from an existing checkout at `c5f655d` with local changes. Origin
resolved to the requested baseline `1195b74c8a77df5cb4d4ed9c669618482f7b078b`.
Validation used a separate detached worktree at that commit; the original tracked
and untracked changes were preserved. Changes and evidence were packaged for
review in a ZIP rather than committed or pushed, as requested. No release was
published.

Execution was directly on the host as UID 1000, without elevation. The active
`/etc/os-release` identified Bazzite, `systemd-detect-virt --container` returned
`none`, neither container marker existed, and PID 1's cgroup was `/init.scope`.
All builds, driver probes, and hardware tests ran in that same environment.

| Component                   | Observed version/identity                                                                                                                   |
| --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| OS                          | Bazzite `44.20260929.0 (Kinoite)`, variant `bazzite-nvidia-open`                                                                            |
| Kernel                      | `7.2.7-ogc1.1.fc44.x86_64`                                                                                                                  |
| Architecture / libc         | `x86_64`, Node `x64`, glibc `2.43`; selected `x86_64-unknown-linux-gnu`                                                                     |
| NVIDIA GPU                  | GeForce RTX 4060 Ti, PCI `0000:01:00.0`, device `10de:2803`, subsystem `1458:4123`, Ada Lovelace                                            |
| NVIDIA kernel driver / NVML | `615.71.09` / `13.615.71.09`                                                                                                                |
| Active NVML library         | `/usr/lib64/libnvidia-ml.so.615.71.09`, confirmed in the actual Node test's mapped modules                                                  |
| Intel GPU / kernel driver   | No Intel display-class PCI function or DRM GPU exposed; neither `i915` nor `xe` bound to an Intel GPU                                       |
| Level Zero loader / runtime | `libze_loader.so.1` and `libze_intel_gpu.so.1` unavailable to the dynamic loader; no loader/compute-runtime RPM found; versions unavailable |
| Tooling                     | Node `26.7.0`, installed pnpm `11.19.0` (repository declares `10.32.1`), Rust/Cargo `1.95.0`, GCC `16.2.1`, libdrm `2.4.134`                |

The ordinary user successfully opened `/dev/dri/card1`, `/dev/dri/renderD128`,
`/dev/nvidia0`, `/dev/nvidiactl`, and NVIDIA modeset/UVM nodes for read/write.
NVML initialized successfully through its API. The actual Node validator also
opened the discovered DRM nodes and confirmed the mapped NVML library. Inventory
and diagnostics correlated sysfs and NVML into exactly one physical GPU, stable
ID `gpu_nvidia_aec494a8cd150f480b35`, with no duplicate PCI or DRM devices.
The absent Level Zero library remained an optional `driver-library-missing`
diagnostic with zero matches and did not prevent inventory, NVML, or shutdown.
No driver, permission, BIOS, or runtime configuration was changed.

| Requested command                                        | Result                                                                                                                                                                  |
| -------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `pnpm install --frozen-lockfile --ignore-scripts`        | Passed with `CI=true`; lockfile unchanged                                                                                                                               |
| `pnpm check`                                             | Passed initially and after changes: format, lint, typecheck, 20 TypeScript tests, 71 core + 2 NAPI Rust tests, strict Clippy, six-target configuration, subprocess scan |
| `pnpm native:build:release`                              | Passed, native x64 glibc release addon                                                                                                                                  |
| `pnpm build`                                             | Passed, public ESM/CommonJS and declarations                                                                                                                            |
| `pnpm native:validate-artifact x86_64-unknown-linux-gnu` | Passed, 2,291,192-byte x64 ELF addon                                                                                                                                    |
| `pnpm native:test-loader`                                | Passed, ESM/CommonJS each discovered one GPU and closed                                                                                                                 |
| `pnpm pack:test-local`                                   | Passed, local 0.2.0 tarball; ESM/CommonJS each discovered one GPU, sampled batches, aborted streams, and exited                                                         |
| `pnpm test:intel-hardware --require-intel-telemetry`     | Failed all five consecutive initial runs and all five after test changes: `Intel release validation requires a Windows or Linux Intel host`                             |
| `pnpm test:linux-hardware`                               | Skipped all five consecutive initial runs and all five after test changes: `requires at least one Intel GPU`                                                            |

The last two rows are missing hardware/runtime prerequisites, not a demonstrated
telemetry-library defect. Both required tests could not be confirmed unskipped;
Linux Intel/Sysman and hybrid sign-off remain blocked. Sysfs-only readings would
not satisfy Sysman validation. No Intel occupancy, measured Intel intervals,
Sysman sensors/clocks/energy, or i915/Xe correlation is claimed here.
The existing Windows watchdog outlier remains unresolved; this host supplies no
new evidence about its cause.

Separate supplemental NVIDIA validation ran five consecutive times with a
30-second watchdog, preserving every JSON result and stderr phase log. All five
passed without skips or timeouts, with child exit in 645–811 ms. This validator
is saved with the evidence and exercises the public API without changing the
Intel or hybrid prerequisite checks.

Representative NVIDIA observations from `nvidia-report-1.json`:

| Reading                                    | Source / quality           | Observation                                                                                          |
| ------------------------------------------ | -------------------------- | ---------------------------------------------------------------------------------------------------- |
| Overall / memory-controller utilization    | `nvml` / direct            | `10%` / `8%`; NVML internal sample period, no synthetic measured interval                            |
| Memory                                     | `nvml` / direct            | `17,175,674,880` total bytes; `2,342,125,568` used bytes                                             |
| Temperature                                | `nvml` / direct            | GPU core `41 °C`; other temperature fields omitted                                                   |
| Power / limit / energy                     | `nvml` / direct            | `32.222 W`, `165 W`, `153,070.881 J` cumulative energy                                               |
| Graphics / compute / memory / video clocks | `nvml` / direct            | `2565` / `2565` / `8751` / `2085 MHz`                                                                |
| Fan                                        | `nvml` / direct            | `30%`, `1000 RPM`                                                                                    |
| Encoder / decoder                          | `nvml` / direct            | Available idle `0%` with NVML-reported `100 ms` intervals                                            |
| Processes                                  | `nvml` / direct memory     | 12 requested process entries with framebuffer allocations; per-process utilization omitted           |
| Unsupported fields                         | Capability false / omitted | Graphics/compute/copy utilization, extra GPU temperatures, shared/unified memory; no fabricated zero |

Batch and per-GPU streams ran concurrently at requested delivery intervals of
200 ms and 500 ms, with process inclusion enabled only on the batch. Timestamps
advanced on both; encoder/decoder retained their own 100 ms NVML intervals.
The four 60-second pending batch streams alternated process settings: enabled
streams included processes, disabled streams omitted them. Filesystem reads
completed in 0.089–0.110 ms, abort of all four reads in 0.223–0.242 ms, and close
with a pending batch read in 2.181–3.436 ms. Batch/per-GPU early breaks,
idempotent close, use-after-close rejection, refresh, reopen, and successful
worker exit passed. Worker telemetry and main-monitor telemetry after worker
close retained the available NVML fields and stable IDs.

Test defects fixed during this work:

- The Intel child previously emitted no JSON on assertion/prerequisite failure.
  It now saves discovered devices, initial diagnostics, and a structured failure
  before exiting nonzero. Assertions and required telemetry semantics remain
  unchanged; stderr carries phase markers and the original error.
- Linux timeout errors now preserve captured child stderr, and the parent waits
  for `close` so output pipes drain before JSON parsing. Child phases are marked.
- Linux worker isolation now waits for a successful worker exit and rejects
  missing inventory, rather than resolving immediately on its message.

A direct probe of the revised Linux worker helper passed on the NVIDIA GPU.
A deliberately hanging fixture verified the Linux parent's existing 30-second
watchdog kills the child and retains its phase marker (30,053 ms including
process cleanup). That was a synthetic watchdog check, not a hardware timeout.
No actual hardware watchdog expired; no deadline was increased and no
retry-to-pass logic was used. No telemetry runtime code or loader policy changed.
The full quality gate still verifies optional fallback, loader security, explicit
zero/unavailable semantics, and absence of telemetry subprocesses.

All logs and reports are in `artifacts/bazzite-gpu-validation/` in the review ZIP.
Direct Node invocations separated stdout JSON from stderr:

```sh
node scripts/test-intel-hardware.mjs --require-intel-telemetry > artifacts/bazzite-gpu-validation/intel-report.json 2> artifacts/bazzite-gpu-validation/intel-report.stderr.log
node scripts/test-linux-hardware.mjs > artifacts/bazzite-gpu-validation/linux-report.json 2> artifacts/bazzite-gpu-validation/linux-report.stderr.log
```

Both final files parsed as JSON: Intel reported `failed: true` with exit code 1;
Linux reported `skipped: true`. The initial empty Intel stdout and its stderr are
retained as `intel-report-initial.*`; initial and final repeat logs are retained
separately. `host.json`, `commands.json`, `direct-report-final-outcomes.json`,
`nvidia-report-1.json` through `nvidia-report-5.json`,
`nvidia-repeat-results.json`, and the supplemental validator sources provide the
remaining evidence. The first supplemental draft had an incorrect test reference
to `identity.memory` rather than `snapshot.memory`; its failure log is retained
as `nvidia-initial-report.*`. This was corrected before the five-run series and
was not a library defect.

Still untested: Linux Intel hardware/Sysman, hybrid operation, real missing-NVML
fallback, permission-denied driver calls, AMD, multiple NVIDIA devices,
MIG/vGPU/SR-IOV, device reset/removal, hot-unplug, sleep/wake, ARM64 glibc, and musl
hardware. Deterministic coverage does not replace these hardware claims.
