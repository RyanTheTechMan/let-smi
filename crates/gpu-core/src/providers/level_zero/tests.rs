use super::*;
#[test]
fn counters_reprime_after_reset() {
    let mut baseline = None;
    assert!(counter_delta(&mut baseline, (10, 100)).is_err());
    assert_eq!(counter_delta(&mut baseline, (20, 200)).unwrap(), (10, 100));
    assert_eq!(
        reason(&counter_delta(&mut baseline, (1, 300)).unwrap_err()),
        UnavailableReason::FirstSample
    );
    assert_eq!(counter_delta(&mut baseline, (1, 400)).unwrap(), (0, 100));
}

#[test]
fn enumeration_is_bounded_and_retries_growth() {
    let oversized = enumerate(
        |count, _| {
            unsafe { *count = 129 };
            ZE_RESULT_SUCCESS
        },
        128,
    );
    assert!(oversized.is_err());
    let mut calls = 0;
    let result = enumerate(
        |count, items| {
            calls += 1;
            unsafe {
                if items.is_null() {
                    *count = 1;
                } else if calls == 2 {
                    *count = 2;
                } else {
                    *items = 1;
                    *items.add(1) = 2;
                    *count = 2;
                }
            }
            ZE_RESULT_SUCCESS
        },
        128,
    )
    .unwrap();
    assert_eq!(result, vec![1, 2]);
    assert_eq!(calls, 3);
    assert!(
        enumerate(
            |count, _| {
                unsafe { *count = 1 };
                ZE_RESULT_SUCCESS
            },
            128
        )
        .is_err()
    );
}

#[test]
fn error_reasons_and_pci_identity_are_explicit() {
    for (code, expected) in [
        (
            ZE_RESULT_ERROR_INSUFFICIENT_PERMISSIONS,
            UnavailableReason::PermissionDenied,
        ),
        (
            ZE_RESULT_ERROR_UNSUPPORTED_FEATURE,
            UnavailableReason::Unsupported,
        ),
        (ZE_RESULT_ERROR_DEVICE_LOST, UnavailableReason::DeviceLost),
    ] {
        assert_eq!(reason(&check(code).unwrap_err()), expected);
    }
    assert_eq!(
        pci_address(zes_pci_address_t {
            domain: 0,
            bus: 1,
            device: 2,
            function: 3
        })
        .unwrap(),
        "0000:01:02.3"
    );
    assert!(
        pci_address(zes_pci_address_t {
            domain: 0,
            bus: 256,
            device: 2,
            function: 3
        })
        .is_err()
    );
    assert_eq!(engine_metric(ZES_ENGINE_GROUP_MEDIA_ALL), None);
    assert_eq!(engine_metric(ZES_ENGINE_GROUP_MEDIA_CODEC_SINGLE), None);
}

macro_rules! unsupported {
    ($name:ident($($arg:ident: $type:ty),*)) => {
        unsafe extern "C" fn $name($($arg: $type),*) -> ze_result_t {
            $(let _ = $arg;)* ZE_RESULT_ERROR_UNSUPPORTED_FEATURE
        }
    };
}
unsupported!(no_frequency_properties(handle: zes_freq_handle_t, output: *mut zes_freq_properties_t));
unsupported!(no_frequency(handle: zes_freq_handle_t, output: *mut zes_freq_state_t));
unsupported!(no_temperature_properties(handle: zes_temp_handle_t, output: *mut zes_temp_properties_t));
unsupported!(no_temperature(handle: zes_temp_handle_t, output: *mut f64));
unsupported!(no_power_properties(handle: zes_pwr_handle_t, output: *mut zes_power_properties_t));
unsupported!(no_energy(handle: zes_pwr_handle_t, output: *mut zes_power_energy_counter_t));

unsafe extern "C" fn init(_: u32) -> ze_result_t {
    ZE_RESULT_SUCCESS
}
unsafe extern "C" fn drivers(count: *mut u32, output: *mut zes_driver_handle_t) -> ze_result_t {
    unsafe {
        *count = 1;
        if !output.is_null() {
            *output = 1usize as _;
        }
    }
    ZE_RESULT_SUCCESS
}
unsafe extern "C" fn devices(
    _: zes_driver_handle_t,
    count: *mut u32,
    output: *mut zes_device_handle_t,
) -> ze_result_t {
    unsafe {
        *count = 1;
        if !output.is_null() {
            *output = 2usize as _;
        }
    }
    ZE_RESULT_SUCCESS
}
unsafe extern "C" fn properties(
    _: zes_device_handle_t,
    output: *mut zes_device_properties_t,
) -> ze_result_t {
    unsafe {
        (*output).core.vendorId = 0x8086;
    }
    ZE_RESULT_SUCCESS
}
unsafe extern "C" fn pci(_: zes_device_handle_t, output: *mut zes_pci_properties_t) -> ze_result_t {
    unsafe {
        (*output).address = zes_pci_address_t {
            domain: 0,
            bus: 0,
            device: 2,
            function: 0,
        };
    }
    ZE_RESULT_SUCCESS
}
unsafe extern "C" fn engines(
    _: zes_device_handle_t,
    count: *mut u32,
    output: *mut zes_engine_handle_t,
) -> ze_result_t {
    unsafe {
        *count = 1;
        if !output.is_null() {
            *output = 3usize as _;
        }
    }
    ZE_RESULT_SUCCESS
}
unsafe extern "C" fn engine_properties(
    _: zes_engine_handle_t,
    output: *mut zes_engine_properties_t,
) -> ze_result_t {
    unsafe {
        (*output).type_ = ZES_ENGINE_GROUP_ALL;
    }
    ZE_RESULT_SUCCESS
}
std::thread_local! { static TIME: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
unsafe extern "C" fn activity(
    _: zes_engine_handle_t,
    output: *mut zes_engine_stats_t,
) -> ze_result_t {
    TIME.with(|time| {
        time.set(time.get() + 100_000);
        unsafe {
            (*output).timestamp = time.get();
            (*output).activeTime = time.get() / 4;
        }
    });
    ZE_RESULT_SUCCESS
}
macro_rules! empty_enumeration {
    ($name:ident, $type:ty) => {
        unsafe extern "C" fn $name(
            _: zes_device_handle_t,
            count: *mut u32,
            _: *mut $type,
        ) -> ze_result_t {
            unsafe {
                *count = 0;
            }
            ZE_RESULT_SUCCESS
        }
    };
}
empty_enumeration!(frequencies, zes_freq_handle_t);
empty_enumeration!(temperatures, zes_temp_handle_t);
empty_enumeration!(powers, zes_pwr_handle_t);

fn fixture_api() -> Api {
    Api::fixture(
        init,
        drivers,
        devices,
        properties,
        pci,
        engines,
        engine_properties,
        activity,
        frequencies,
        no_frequency_properties,
        no_frequency,
        temperatures,
        no_temperature_properties,
        no_temperature,
        powers,
        no_power_properties,
        no_energy,
    )
}

#[test]
fn injected_runtime_probes_partial_support_and_measures_activity() {
    let api = Arc::new(fixture_api());
    let provider = LevelZeroProvider {
        state: Mutex::new(State {
            api: Some(api.clone()),
            failure: None,
            records: BTreeMap::new(),
            warnings: Vec::new(),
        }),
    };
    assert!(provider.enumerate().unwrap().is_empty());
    assert_eq!(provider.diagnostic().devices_matched, 1);
    let mut state = provider.state.lock();
    let record = state.records.get_mut("0000:00:02.0").unwrap();
    assert!(record.capabilities.supports(MetricKey::UtilizationOverall));
    assert!(
        !record
            .capabilities
            .supports(MetricKey::TemperatureCoreCelsius)
    );
    let first = sample_record(&api, record, "intel-test", &SampleRequest::default());
    assert_eq!(first.unavailable[0].reason, UnavailableReason::FirstSample);
    record.engines[0].observed_at = Some(Instant::now() - std::time::Duration::from_millis(250));
    let second = sample_record(&api, record, "intel-test", &SampleRequest::default());
    assert_eq!(second.metrics[0].value.as_f64(), Some(25.0));
    // Native counters advanced by 100,000 implementation-specific ticks, while
    // the monotonic observation interval was 250 ms. Do not guess tick units.
    assert!(second.metrics[0].interval_ms.unwrap() >= 250);
    drop(state);
    provider.shutdown();
    assert!(!provider.diagnostic().loaded);
}

#[test]
fn sensor_zero_is_preserved_and_power_units_are_correct() {
    let mut sample = ProviderSample::default();
    emit_max(
        &mut sample,
        "intel-test",
        MetricKey::TemperatureCoreCelsius,
        [Ok((0.0, 0))],
        MetricQuality::Direct,
        "test",
    );
    assert_eq!(sample.metrics[0].value.as_f64(), Some(0.0));
    let mut baseline = Some((1_000_000, 1_000_000));
    let (energy, time) = counter_delta(&mut baseline, (3_000_000, 2_000_000)).unwrap();
    assert_eq!(energy as f64 / time as f64, 2.0);
    assert_eq!(3_000_000.0 / 1_000_000.0, 3.0);
}

std::thread_local! {
    static POWER_DOMAIN: std::cell::Cell<u32> = const { std::cell::Cell::new(ZES_POWER_DOMAIN_GPU) };
    static DENY_TEMPERATURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
unsafe extern "C" fn integrated_properties(
    _: zes_device_handle_t,
    output: *mut zes_device_properties_t,
) -> ze_result_t {
    unsafe {
        (*output).core.vendorId = 0x8086;
        (*output).core.flags = 1;
    }
    ZE_RESULT_SUCCESS
}
unsafe extern "C" fn one_power(
    _: zes_device_handle_t,
    count: *mut u32,
    output: *mut zes_pwr_handle_t,
) -> ze_result_t {
    unsafe {
        *count = 1;
        if !output.is_null() {
            *output = 4usize as _;
        }
    }
    ZE_RESULT_SUCCESS
}
unsafe extern "C" fn gpu_power_properties(
    _: zes_pwr_handle_t,
    output: *mut zes_power_properties_t,
) -> ze_result_t {
    unsafe {
        let extension = (*output).pNext.cast::<zes_power_ext_properties_t>();
        (*extension).domain = POWER_DOMAIN.with(std::cell::Cell::get);
    }
    ZE_RESULT_SUCCESS
}
unsafe extern "C" fn gpu_energy(
    _: zes_pwr_handle_t,
    output: *mut zes_power_energy_counter_t,
) -> ze_result_t {
    unsafe {
        (*output).timestamp = TIME.with(std::cell::Cell::get);
        (*output).energy = (*output).timestamp * 2;
    }
    ZE_RESULT_SUCCESS
}
unsafe extern "C" fn one_temperature(
    _: zes_device_handle_t,
    count: *mut u32,
    output: *mut zes_temp_handle_t,
) -> ze_result_t {
    unsafe {
        *count = 1;
        if !output.is_null() {
            *output = 5usize as _;
        }
    }
    ZE_RESULT_SUCCESS
}
unsafe extern "C" fn gpu_temperature_properties(
    _: zes_temp_handle_t,
    output: *mut zes_temp_properties_t,
) -> ze_result_t {
    unsafe {
        (*output).type_ = ZES_TEMP_SENSORS_GPU;
    }
    ZE_RESULT_SUCCESS
}
unsafe extern "C" fn gpu_temperature(_: zes_temp_handle_t, output: *mut f64) -> ze_result_t {
    if DENY_TEMPERATURE.with(std::cell::Cell::get) {
        return ZE_RESULT_ERROR_INSUFFICIENT_PERMISSIONS;
    }
    unsafe {
        *output = 0.0;
    }
    ZE_RESULT_SUCCESS
}

#[test]
fn power_requires_gpu_attribution_and_sensor_failure_is_independent() {
    let mut api = fixture_api();
    api.device_properties = integrated_properties;
    api.powers = one_power;
    api.power_properties = gpu_power_properties;
    api.energy = gpu_energy;
    api.temperatures = one_temperature;
    api.temperature_properties = gpu_temperature_properties;
    api.temperature = gpu_temperature;
    for domain in [
        ZES_POWER_DOMAIN_PACKAGE,
        ZES_POWER_DOMAIN_CARD,
        ZES_POWER_DOMAIN_GPU,
    ] {
        POWER_DOMAIN.with(|value| value.set(domain));
        DENY_TEMPERATURE.with(|value| value.set(true));
        let (_, mut record) = discover(&api, 2, &mut Vec::new()).unwrap().unwrap();
        assert_eq!(record.power.is_some(), domain == ZES_POWER_DOMAIN_GPU);
        assert!(
            record
                .capabilities
                .supports(MetricKey::TemperatureCoreCelsius)
        );
        let first = sample_record(&api, &mut record, "intel", &SampleRequest::default());
        assert!(
            first
                .unavailable
                .iter()
                .any(|value| value.metric == MetricKey::TemperatureCoreCelsius
                    && value.reason == UnavailableReason::PermissionDenied)
        );
        DENY_TEMPERATURE.with(|value| value.set(false));
        let second = sample_record(&api, &mut record, "intel", &SampleRequest::default());
        assert!(
            second
                .metrics
                .iter()
                .any(|value| value.metric == MetricKey::TemperatureCoreCelsius
                    && value.value.as_f64() == Some(0.0))
        );
        if domain == ZES_POWER_DOMAIN_GPU {
            assert!(
                first
                    .unavailable
                    .iter()
                    .any(|value| value.metric == MetricKey::PowerDrawWatts
                        && value.reason == UnavailableReason::FirstSample)
            );
            assert!(
                second
                    .metrics
                    .iter()
                    .any(|value| value.metric == MetricKey::PowerDrawWatts
                        && value.value.as_f64() == Some(2.0)
                        && value.interval_ms == Some(100))
            );
            assert!(
                second
                    .metrics
                    .iter()
                    .any(|value| value.metric == MetricKey::PowerEnergyJoules
                        && value.value.as_f64()
                            == Some(TIME.with(std::cell::Cell::get) as f64 * 2.0 / 1_000_000.0))
            );
        }
    }
}

unsafe extern "C" fn group_activity(
    handle: zes_engine_handle_t,
    output: *mut zes_engine_stats_t,
) -> ze_result_t {
    let percent = if handle as usize == 3 { 25 } else { 80 };
    unsafe {
        (*output).timestamp = TIME.with(std::cell::Cell::get);
        (*output).activeTime = (*output).timestamp * percent / 100;
    }
    ZE_RESULT_SUCCESS
}

#[test]
fn engines_prefer_device_aggregate_and_never_sum_groups() {
    let mut api = fixture_api();
    let (_, mut record) = discover(&api, 2, &mut Vec::new()).unwrap().unwrap();
    record.engines.push(Engine {
        handle: 6,
        group: ZES_ENGINE_GROUP_COMPUTE_ALL,
        on_subdevice: false,
        subdevice_id: 0,
        baseline: None,
        observed_at: None,
        last: None,
    });
    record.capabilities.insert(MetricKey::UtilizationCompute);
    api.activity = group_activity;
    TIME.with(|value| value.set(100_000));
    sample_record(&api, &mut record, "intel", &SampleRequest::default());
    TIME.with(|value| value.set(200_000));
    let aggregate = sample_record(&api, &mut record, "intel", &SampleRequest::default());
    assert_eq!(
        aggregate
            .metrics
            .iter()
            .find(|value| value.metric == MetricKey::UtilizationOverall)
            .unwrap()
            .value
            .as_f64(),
        Some(25.0)
    );
    assert_eq!(
        aggregate
            .metrics
            .iter()
            .find(|value| value.metric == MetricKey::UtilizationCompute)
            .unwrap()
            .value
            .as_f64(),
        Some(80.0)
    );
    record.engines[0].group = ZES_ENGINE_GROUP_COPY_ALL;
    TIME.with(|value| value.set(300_000));
    let fallback = sample_record(&api, &mut record, "intel", &SampleRequest::default());
    let overall = fallback
        .metrics
        .iter()
        .find(|value| value.metric == MetricKey::UtilizationOverall)
        .unwrap();
    assert_eq!(overall.value.as_f64(), Some(80.0));
    assert!(overall.definition.as_ref().unwrap().contains("not summed"));
    TIME.with(|value| value.set(0));
    let reset = sample_record(&api, &mut record, "intel", &SampleRequest::default());
    assert!(
        reset
            .unavailable
            .iter()
            .all(|value| value.reason == UnavailableReason::FirstSample)
    );
}
