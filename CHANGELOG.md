# Changelog

## 0.2.0 — Unreleased

- Add `GpuMonitor.sampleAll()` and `GpuMonitor.samplesAll()` for native collection
  of every GPU, sharing polls with per-GPU consumers.
- Keep streams and refresh responsive during one-shot counter warmup; separate
  process enrichment from scalar polling and collect PDH once per adapter batch.
- Add optional Intel Level Zero Sysman telemetry on Windows/Linux: engine
  occupancy, clocks, GPU/memory temperature, and attributable GPU energy/power.
  Runtime probes preserve PDH/sysfs fallbacks and explicit unavailable readings.
- Add deterministic counter/concurrency tests and a required Intel hardware
  validation command. Windows/Linux Intel hardware sign-off is pending.

## 0.1.0

Initial cross-platform GPU discovery and telemetry release.
