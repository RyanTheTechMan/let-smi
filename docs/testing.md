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
- NVML conversions and unavailable-platform behavior without an NVIDIA GPU;
- IOReport state/energy calculations and AppleSMC key decoding without relying
  on a particular sensor value;
- subscription coalescing, cancellation wake-up, monitor-close wake-up, and
  idempotent provider shutdown;
- bounded sampler commands/subscriptions, one in-flight `next()`, nonblocking
  finalization, and close-aware queued calls;
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

## Observed Windows x64 validation — 2026-08-12

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

## Observed Apple Silicon validation — 2026-08-12

Hardware-tested on an Apple M5 Max running macOS 27 as a normal user. Metal
discovered one stable Apple GPU. The host exposed 11,884 total IOReport channels
before filtering, including the supported GPU residency and energy channels;
the provider retained only its bounded GPU subset. A measured sample returned
derived utilization, power, and energy from IOReport plus an estimated
temperature from 64 AppleSMC GPU die sensors. Public ESM and CommonJS loading,
the arm64 Mach-O artifact check, the complete Rust/NAPI test suite, and strict
Clippy all passed without `sudo` or an external executable.
