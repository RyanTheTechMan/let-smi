#!/bin/sh
# Maintainer-only generation: bindgen-cli 0.72.1 and libclang are required.
# Headers: oneapi-src/level-zero v1.28.2, 6369d8d642e9c7625e67f38664267f171b8e42dc.
set -eu
cd "$(dirname "$0")/.."
RUST_LOG=error bindgen crates/gpu-core/src/providers/level_zero/vendor/zes_api.h \
  --allowlist-type 'ze_result_t|zes_(init_flags_t|driver_handle_t|device_handle_t|engine_handle_t|freq_handle_t|temp_handle_t|pwr_handle_t|device_properties_t|pci_properties_t|engine_properties_t|engine_stats_t|freq_properties_t|freq_state_t|temp_properties_t|power_properties_t|power_ext_properties_t|power_energy_counter_t)' \
  --generate types,vars --no-prepend-enum-name --no-doc-comments --with-derive-default \
  --output crates/gpu-core/src/providers/level_zero/ffi.rs
