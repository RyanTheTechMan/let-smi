# Changelog

## 0.2.0 — Unreleased

- Add `GpuMonitor.sampleAll()` and `GpuMonitor.samplesAll()` for native collection
  of every GPU, sharing polls with per-GPU consumers.
- Keep streams and refresh responsive during one-shot counter warmup; separate
  process enrichment from scalar polling and collect PDH once per adapter batch.
- Add optional Intel Level Zero Sysman telemetry on Windows/Linux: engine
  occupancy, clocks, GPU/memory temperature, and attributable GPU energy/power.
  Runtime probes preserve PDH/sysfs fallbacks and explicit unavailable readings.
- Add deterministic counter/concurrency tests, Intel/Linux failure and phase
  evidence, and checks for successful worker exit. Watchdog deadlines are unchanged.
- Verify batch inventory and stream cancellation in ESM/CommonJS registry smoke
  tests after publication.

### Hardware coverage and known limitations

- Windows UHD 770 Sysman utilization, compute occupancy, and GPU frequency were
  validated alongside RTX 4070 Ti NVML. Intel temperature/power are unsupported on
  that driver and remain unavailable.
- Linux RTX 4060 Ti NVML and sampling lifecycle checks passed five consecutive
  runs on Bazzite. That host exposed no Intel GPU or Level Zero runtime, so Linux
  Intel/Sysman and Intel/NVIDIA hybrid operation remain unvalidated.
- One Windows hybrid run exceeded the existing 20-second watchdog. Subsequent
  instrumented runs passed, but the original cause remains unexplained.

Version 0.2.0 is prepared for release with these limitations documented. Missing
optional runtimes continue to preserve discovery and other providers. See
[hardware validation](docs/testing.md) for the evidence and untested scenarios.

## 0.1.0

Initial cross-platform GPU discovery and telemetry release.
