# GPU telemetry engineering plan

## Repository decision

This is a new repository. It will use a pnpm workspace for JavaScript packages and a
Cargo workspace for Rust crates:

- `packages/gpu`: the single public TypeScript package (`let-smi`), including the
  ergonomic class API and native-binary loader.
- `packages/gpu/npm/*`: CI-generated optional platform packages containing only
  a prebuilt `.node` artifact and package metadata.
- `crates/gpu-core`: platform-neutral identity, correlation, metric, merge,
  capability, diagnostics, and sampling logic plus platform-gated providers.
- `crates/gpu-napi`: a data-oriented NAPI-RS boundary with opaque monitor and
  subscription objects.

The public schema is owned by this project. Native provider types never cross the
Node boundary.

## Architecture

1. Providers emit partial device and metric observations. Optional vendor runtimes
   are opened at runtime and their absence becomes diagnostics, never a load error.
2. The core correlates observations by strong identifiers (UUID, PCI address, LUID,
   registry IDs, PNP ID), derives deterministic IDs, and only uses names/ordinals as
   an explicitly low-confidence fallback.
3. Metric candidates are scored per field using availability, quality, provider
   specificity/reliability, and freshness. The chosen value retains provenance and
   merge decisions are inspectable.
4. A monitor owns provider handles, counter baselines, a shared sampler, topology,
   and shutdown. JavaScript listeners never own or mutate native counter state.
5. TypeScript constructs `NvidiaGpu`, `AmdGpu`, `IntelGpu`, `AppleGpu`, or
   `UnknownGpu` around a private native monitor client. Streams are cancellable and
   monitor closure is idempotent.

## Delivery sequence

1. Canonical Rust model, provider traits, correlation, merge scoring, mocks, and
   deterministic unit tests.
2. Native inventory: Linux PCI/DRM/sysfs, macOS Metal/IOKit, Windows DXGI; generic
   counter telemetry where reliable.
3. Dynamically loaded vendor providers, beginning with NVML, followed by guarded
   ADLX, Level Zero, AMD Linux, and Apple private telemetry boundaries. Hardware-only
   paths get mocks and clean unavailable diagnostics when they cannot be exercised.
4. NAPI bindings, TypeScript classes, shared continuous sampling, refresh,
   diagnostics, vendor extensions, and package loader.
5. CI/prebuild metadata, documentation, integration-test skips, and full local
   verification.

## Safety and compatibility rules

- No provider invokes a command-line program.
- Missing data is an explicit unavailable state; numeric zero is never a sentinel.
- Optional GPU libraries are dynamically loaded and kept behind provider modules.
- Stable IDs never depend solely on enumeration order.
- Immutable identity is cached; sampling avoids repeated topology scans.
- Unsupported platforms/providers remain loadable and explain themselves through
  diagnostics.
- Platform-specific code is compile-gated so every supported target can be built
  without foreign SDK libraries installed on the build host.

## Verification gates

Formatting, ESLint, TypeScript type checking, Vitest, Cargo tests, Clippy with
warnings denied, native addon build, package build, package-content inspection, and
a source scan proving there are no subprocess provider calls. Hardware integration
tests skip when matching hardware or runtime libraries are absent.
