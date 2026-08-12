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
  idempotent provider shutdown.

Platform code is type-checked for Windows x64/ARM64, Linux x64/ARM64, and macOS
x64/ARM64. Linux musl is built in the native artifact matrix.

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

| Platform            | Required lab coverage                                                                                                                  |
| ------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| Windows             | NVIDIA-only; AMD-only; Intel integrated; Intel+NVIDIA; Intel+AMD; multiple NVIDIA; multiple AMD; ARM64 inventory where hardware exists |
| Linux               | NVIDIA; AMD; Intel i915; Intel Xe; hybrid; headless; x64 glibc; ARM64 glibc; x64 musl load test                                        |
| macOS               | Apple M1, M2, M3, M4-or-newer; Intel integrated; Intel Mac with AMD discrete where available                                           |
| Virtual/partitioned | VM/no GPU; disabled device; NVIDIA MIG; vGPU; SR-IOV; eGPU attach/detach                                                               |
| Failure modes       | missing vendor libraries; permission-denied sysfs; reset/device lost; sleep/wake; driver reload; private Apple API unavailable         |

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
