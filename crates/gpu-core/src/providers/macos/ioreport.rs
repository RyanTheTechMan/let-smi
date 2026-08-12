// IOReport is a private Apple API. This adapter is independently structured
// around the public behavior researched in macmon (MIT) and loads every private
// symbol dynamically so inventory survives when the API is absent.

use crate::error::{GpuError, Result};
use crate::model::{
    CanonicalGpu, CapabilitySet, DeviceObservation, GpuKind, GpuVendor, MetricKey,
    MetricObservation, MetricQuality, MetricValue, ProviderDiagnostic, ProviderSample,
    SampleRequest, UnavailableObservation, UnavailableReason, now_millis,
};
use crate::provider::{InventoryProvider, ProviderMetadata, TelemetryProvider};
use core_foundation_sys::array::{
    CFArrayAppendValue, CFArrayCreateMutable, CFArrayGetCount, CFArrayGetTypeID,
    CFArrayGetValueAtIndex, kCFTypeArrayCallBacks,
};
use core_foundation_sys::base::{
    CFGetTypeID, CFIndex, CFRelease, CFTypeID, CFTypeRef, kCFAllocatorDefault,
};
use core_foundation_sys::dictionary::{
    CFDictionaryCreateMutableCopy, CFDictionaryGetCount, CFDictionaryGetTypeID,
    CFDictionaryGetValue, CFDictionaryRef, CFDictionarySetValue, CFMutableDictionaryRef,
};
use core_foundation_sys::string::{
    CFStringCreateWithBytes, CFStringGetCString, CFStringGetLength,
    CFStringGetMaximumSizeForEncoding, CFStringGetTypeID, CFStringRef, kCFStringEncodingUTF8,
};
use libloading::Library;
use parking_lot::Mutex;
use std::ffi::c_void;
use std::ptr;
use std::time::Instant;

const PROVIDER_ID: &str = "apple-ioreport";
const MAX_VALIDATED_MACOS_MAJOR: u32 = 27;
const MAX_IOREPORT_DICTIONARY_ENTRIES: usize = 4_096;
// Modern high-end Apple Silicon systems expose well over ten thousand total
// IOReport channels before the provider filters down to its small GPU subset.
// Keep enumeration bounded while allowing current hardware inventories.
const MAX_IOREPORT_CHANNELS: usize = 65_536;
const MAX_SELECTED_IOREPORT_CHANNELS: usize = 256;
const MAX_IOREPORT_STATES: usize = 256;
const MAX_CF_STRING_UNITS: usize = 4_096;
const MAX_CF_STRING_BYTES: usize = 16_384;
const MAX_SYSCTL_VERSION_BYTES: usize = 256;

type SubscriptionRef = *const c_void;
type CopyAllChannelsFn = unsafe extern "C" fn(u64, u64) -> CFDictionaryRef;
type CreateSubscriptionFn = unsafe extern "C" fn(
    *const c_void,
    CFMutableDictionaryRef,
    *mut CFMutableDictionaryRef,
    u64,
    CFTypeRef,
) -> SubscriptionRef;
type CreateSamplesFn =
    unsafe extern "C" fn(SubscriptionRef, CFMutableDictionaryRef, CFTypeRef) -> CFDictionaryRef;
type CreateSamplesDeltaFn =
    unsafe extern "C" fn(CFDictionaryRef, CFDictionaryRef, CFTypeRef) -> CFDictionaryRef;
type ChannelStringFn = unsafe extern "C" fn(CFDictionaryRef) -> CFStringRef;
type SimpleIntegerFn = unsafe extern "C" fn(CFDictionaryRef, i32) -> i64;
type StateCountFn = unsafe extern "C" fn(CFDictionaryRef) -> i32;
type StateNameFn = unsafe extern "C" fn(CFDictionaryRef, i32) -> CFStringRef;
type StateResidencyFn = unsafe extern "C" fn(CFDictionaryRef, i32) -> i64;

struct Api {
    copy_all_channels: CopyAllChannelsFn,
    create_subscription: CreateSubscriptionFn,
    create_samples: CreateSamplesFn,
    create_samples_delta: CreateSamplesDeltaFn,
    channel_group: ChannelStringFn,
    channel_subgroup: ChannelStringFn,
    channel_name: ChannelStringFn,
    channel_unit: ChannelStringFn,
    simple_integer: SimpleIntegerFn,
    state_count: StateCountFn,
    state_name: StateNameFn,
    state_residency: StateResidencyFn,
    _library: Library,
}

impl Api {
    fn load() -> std::result::Result<Self, String> {
        let library = [
            "/usr/lib/libIOReport.dylib",
            "/System/Library/PrivateFrameworks/IOReport.framework/IOReport",
            "/System/Library/PrivateFrameworks/IOReport.framework/Versions/A/IOReport",
        ]
        .into_iter()
        .find_map(|path| {
            // SAFETY: Only absolute, OS-owned paths are considered. The handle
            // remains alive for every copied function pointer.
            unsafe { Library::new(path).ok() }
        })
        .ok_or_else(|| "the IOReport private framework was not found".to_owned())?;

        // SAFETY: Each requested symbol is copied with the ABI used by all
        // validated macOS versions and the library is retained by Api.
        unsafe {
            Ok(Self {
                copy_all_channels: symbol(&library, b"IOReportCopyAllChannels\0")?,
                create_subscription: symbol(&library, b"IOReportCreateSubscription\0")?,
                create_samples: symbol(&library, b"IOReportCreateSamples\0")?,
                create_samples_delta: symbol(&library, b"IOReportCreateSamplesDelta\0")?,
                channel_group: symbol(&library, b"IOReportChannelGetGroup\0")?,
                channel_subgroup: symbol(&library, b"IOReportChannelGetSubGroup\0")?,
                channel_name: symbol(&library, b"IOReportChannelGetChannelName\0")?,
                channel_unit: symbol(&library, b"IOReportChannelGetUnitLabel\0")?,
                simple_integer: symbol(&library, b"IOReportSimpleGetIntegerValue\0")?,
                state_count: symbol(&library, b"IOReportStateGetCount\0")?,
                state_name: symbol(&library, b"IOReportStateGetNameForIndex\0")?,
                state_residency: symbol(&library, b"IOReportStateGetResidency\0")?,
                _library: library,
            })
        }
    }
}

unsafe fn symbol<T: Copy>(library: &Library, name: &[u8]) -> std::result::Result<T, String> {
    // SAFETY: The caller supplies the ABI type for the named IOReport symbol
    // and holds the Library for at least as long as the copied pointer.
    unsafe {
        library.get::<T>(name).map(|value| *value).map_err(|error| {
            format!(
                "missing IOReport symbol {}: {error}",
                String::from_utf8_lossy(&name[..name.len().saturating_sub(1)])
            )
        })
    }
}

#[derive(Clone)]
struct Channel {
    group: String,
    subgroup: String,
    name: String,
    unit: String,
}

struct IoReport {
    api: Api,
    subscription: SubscriptionRef,
    channels: CFMutableDictionaryRef,
    source_channels: CFDictionaryRef,
    selected_channels: core_foundation_sys::array::CFMutableArrayRef,
    metadata: Vec<Channel>,
    baseline: Option<(CFDictionaryRef, Instant)>,
}

// IOReport references are opaque retained objects. The monitor serializes every
// access through a Mutex and ultimately calls them from its single sampler.
unsafe impl Send for IoReport {}

impl IoReport {
    fn open() -> std::result::Result<Self, String> {
        if let Some(major) = macos_major()
            && major > MAX_VALIDATED_MACOS_MAJOR
        {
            return Err(format!(
                "IOReport is disabled on unvalidated macOS major version {major}"
            ));
        }

        let api = Api::load()?;
        // SAFETY: Function pointers were resolved above. Returned CF objects
        // are checked for null and released in Drop.
        let all = unsafe { (api.copy_all_channels)(0, 0) };
        if all.is_null() {
            return Err("IOReport returned no channels".into());
        }
        if !cf_type_is(all.cast(), unsafe { CFDictionaryGetTypeID() }) {
            // SAFETY: all is a retained object from CopyAllChannels.
            unsafe { CFRelease(all.cast()) };
            return Err("IOReport returned a non-dictionary channel container".into());
        }
        // SAFETY: the runtime type was checked above.
        let dictionary_count = unsafe { CFDictionaryGetCount(all) };
        if let Err(error) = checked_cf_count(
            "IOReport channel dictionary",
            dictionary_count,
            MAX_IOREPORT_DICTIONARY_ENTRIES,
        ) {
            // SAFETY: all is retained.
            unsafe { CFRelease(all.cast()) };
            return Err(error);
        }
        let array_value = dictionary_value(all, "IOReportChannels").ok_or_else(|| {
            // SAFETY: all is a retained object from CopyAllChannels.
            unsafe { CFRelease(all.cast()) };
            "IOReport channel dictionary is malformed".to_owned()
        })?;
        if !cf_type_is(array_value, unsafe { CFArrayGetTypeID() }) {
            // SAFETY: all is retained.
            unsafe { CFRelease(all.cast()) };
            return Err("IOReportChannels is not an array".into());
        }
        let array = array_value.cast();
        // SAFETY: the runtime type was checked above.
        let capacity = match checked_cf_count(
            "IOReport channel array",
            unsafe { CFArrayGetCount(array) },
            MAX_IOREPORT_CHANNELS,
        ) {
            Ok(value) => value,
            Err(error) => {
                // SAFETY: all is retained.
                unsafe { CFRelease(all.cast()) };
                return Err(error);
            }
        };
        // SAFETY: all is a valid CFDictionary and the allocator is the system
        // default. The returned objects are checked and retained.
        let mutable =
            unsafe { CFDictionaryCreateMutableCopy(kCFAllocatorDefault, dictionary_count, all) };
        if mutable.is_null() {
            // SAFETY: all is retained.
            unsafe { CFRelease(all.cast()) };
            return Err("failed to copy IOReport channels".into());
        }
        // SAFETY: callbacks retain selected channel dictionaries.
        let selected = unsafe {
            CFArrayCreateMutable(
                kCFAllocatorDefault,
                capacity,
                &raw const kCFTypeArrayCallBacks,
            )
        };
        if selected.is_null() {
            // SAFETY: both objects are retained.
            unsafe {
                CFRelease(mutable.cast());
                CFRelease(all.cast());
            }
            return Err("failed to create IOReport channel selection".into());
        }

        let mut metadata = Vec::new();
        let mut malformed_item = false;
        let mut selection_overflow = false;
        for index in 0..capacity {
            // SAFETY: index is bounded by the array count.
            let item: CFDictionaryRef = unsafe { CFArrayGetValueAtIndex(array, index) }.cast();
            if !cf_type_is(item.cast(), unsafe { CFDictionaryGetTypeID() }) {
                malformed_item = true;
                break;
            }
            let channel = Channel {
                group: cf_string(
                    // SAFETY: item came from an IOReport channel array.
                    unsafe { (api.channel_group)(item) },
                ),
                subgroup: cf_string(
                    // SAFETY: item came from an IOReport channel array.
                    unsafe { (api.channel_subgroup)(item) },
                ),
                name: cf_string(
                    // SAFETY: item came from an IOReport channel array.
                    unsafe { (api.channel_name)(item) },
                ),
                unit: cf_string(
                    // SAFETY: item came from an IOReport channel array.
                    unsafe { (api.channel_unit)(item) },
                )
                .trim()
                .to_owned(),
            };
            if is_gpu_channel(&channel) {
                if metadata.len() >= MAX_SELECTED_IOREPORT_CHANNELS {
                    selection_overflow = true;
                    break;
                }
                // SAFETY: selected has type callbacks and item remains valid.
                unsafe { CFArrayAppendValue(selected, item.cast()) };
                metadata.push(channel);
            }
        }
        if malformed_item || selection_overflow {
            // SAFETY: all three objects are retained.
            unsafe {
                CFRelease(selected.cast());
                CFRelease(mutable.cast());
                CFRelease(all.cast());
            }
            return Err(if malformed_item {
                "IOReport channel array contained a non-dictionary item".into()
            } else {
                format!("IOReport selected more than {MAX_SELECTED_IOREPORT_CHANNELS} GPU channels")
            });
        }
        if metadata.is_empty() {
            // SAFETY: objects are retained.
            unsafe {
                CFRelease(selected.cast());
                CFRelease(mutable.cast());
                CFRelease(all.cast());
            }
            return Err("no supported GPU IOReport channels were found".into());
        }

        set_dictionary_value(mutable, "IOReportChannels", selected.cast());
        let mut subscribed_channels: CFMutableDictionaryRef = ptr::null_mut();
        // SAFETY: channel dictionary and out pointer are valid for the call.
        let subscription = unsafe {
            (api.create_subscription)(
                ptr::null(),
                mutable,
                &raw mut subscribed_channels,
                0,
                ptr::null(),
            )
        };
        if !subscribed_channels.is_null() {
            // SAFETY: CreateSubscription returned this retained object.
            unsafe { CFRelease(subscribed_channels.cast()) };
        }
        if subscription.is_null() {
            // SAFETY: objects are retained.
            unsafe {
                CFRelease(selected.cast());
                CFRelease(mutable.cast());
                CFRelease(all.cast());
            }
            return Err("IOReport refused the GPU channel subscription".into());
        }

        Ok(Self {
            api,
            subscription,
            channels: mutable,
            source_channels: all,
            selected_channels: selected,
            metadata,
            baseline: None,
        })
    }

    fn has_utilization(&self) -> bool {
        self.metadata
            .iter()
            .any(|channel| channel.group == "GPU Stats")
    }

    fn has_power(&self) -> bool {
        self.metadata
            .iter()
            .any(|channel| channel.group == "Energy Model" && channel.name == "GPU Energy")
    }

    fn take_sample(&mut self, device_id: &str) -> std::result::Result<ProviderSample, String> {
        // SAFETY: subscription and channels are retained for the API lifetime.
        let next =
            unsafe { (self.api.create_samples)(self.subscription, self.channels, ptr::null()) };
        if next.is_null() {
            self.clear_baseline();
            return Err("IOReport failed to create a sample".into());
        }
        let sampled_at = Instant::now();
        let Some((previous, previous_at)) = self.baseline.replace((next, sampled_at)) else {
            return Ok(self.first_sample(device_id));
        };
        let elapsed = sampled_at
            .saturating_duration_since(previous_at)
            .max(std::time::Duration::from_nanos(1));
        // SAFETY: previous and next are valid samples for this subscription.
        let delta = unsafe { (self.api.create_samples_delta)(previous, next, ptr::null()) };
        // SAFETY: previous was retained by CreateSamples and is no longer used.
        unsafe { CFRelease(previous.cast()) };
        if delta.is_null() {
            return Err("IOReport failed to calculate a sample delta".into());
        }

        let result = self.parse_delta(device_id, delta, elapsed);
        // SAFETY: delta is retained by CreateSamplesDelta.
        unsafe { CFRelease(delta.cast()) };
        result
    }

    fn first_sample(&self, device_id: &str) -> ProviderSample {
        let metrics = self.capability_keys();
        ProviderSample {
            unavailable: metrics
                .into_iter()
                .map(|metric| UnavailableObservation {
                    device_id: device_id.into(),
                    metric,
                    reason: UnavailableReason::FirstSample,
                    source: Some(PROVIDER_ID.into()),
                    message: Some("IOReport needs two counter samples to calculate a delta".into()),
                })
                .collect(),
            ..ProviderSample::default()
        }
    }

    fn capability_keys(&self) -> Vec<MetricKey> {
        let mut result = Vec::new();
        if self.has_utilization() {
            result.push(MetricKey::UtilizationOverall);
        }
        if self.has_power() {
            result.push(MetricKey::PowerDrawWatts);
            result.push(MetricKey::PowerEnergyJoules);
        }
        result
    }

    fn parse_delta(
        &self,
        device_id: &str,
        delta: CFDictionaryRef,
        elapsed: std::time::Duration,
    ) -> std::result::Result<ProviderSample, String> {
        let items = dictionary_value(delta, "IOReportChannels")
            .ok_or_else(|| "IOReport delta has no channel array".to_owned())?;
        if !cf_type_is(items, unsafe { CFArrayGetTypeID() }) {
            return Err("IOReport delta channel value is not an array".into());
        }
        let items = items.cast();
        // SAFETY: items was runtime-checked as a CFArray.
        let count = checked_cf_count(
            "IOReport delta channel array",
            unsafe { CFArrayGetCount(items) },
            MAX_SELECTED_IOREPORT_CHANNELS,
        )?;
        if usize::try_from(count).ok() != Some(self.metadata.len()) {
            return Err("IOReport delta channel count changed".into());
        }

        let mut active_ticks = 0_i128;
        let mut total_ticks = 0_i128;
        let mut energy_joules = 0_f64;
        for index in 0..count {
            // SAFETY: index is bounded by count.
            let item: CFDictionaryRef = unsafe { CFArrayGetValueAtIndex(items, index) }.cast();
            if !cf_type_is(item.cast(), unsafe { CFDictionaryGetTypeID() }) {
                return Err("IOReport delta contained a non-dictionary channel".into());
            }
            let channel = &self.metadata[usize::try_from(index)
                .map_err(|_| "IOReport channel index does not fit usize")?];
            if channel.group == "GPU Stats" {
                // SAFETY: item is a state channel selected during discovery.
                let state_count = unsafe { (self.api.state_count)(item) };
                let state_count =
                    checked_i32_count("IOReport state array", state_count, MAX_IOREPORT_STATES)?;
                for state_index in 0..state_count {
                    let state_index = i32::try_from(state_index)
                        .map_err(|_| "IOReport state index does not fit i32")?;
                    // SAFETY: state_index is bounded by state_count.
                    let name = cf_string(unsafe { (self.api.state_name)(item, state_index) });
                    // SAFETY: state_index is bounded by state_count.
                    let residency = unsafe { (self.api.state_residency)(item, state_index) };
                    if residency < 0 {
                        return Err("IOReport returned negative state residency".into());
                    }
                    let residency = i128::from(residency);
                    total_ticks = total_ticks
                        .checked_add(residency)
                        .ok_or_else(|| "IOReport total state residency overflowed".to_owned())?;
                    if is_active_state(&name) {
                        active_ticks = active_ticks.checked_add(residency).ok_or_else(|| {
                            "IOReport active state residency overflowed".to_owned()
                        })?;
                    }
                }
            } else if channel.group == "Energy Model" && channel.name == "GPU Energy" {
                // SAFETY: item is a simple integer energy channel.
                let raw = unsafe { (self.api.simple_integer)(item, 0) };
                if raw < 0 {
                    return Err("IOReport returned negative GPU energy".into());
                }
                energy_joules += energy_to_joules(raw as f64, &channel.unit)?;
                if !energy_joules.is_finite() {
                    return Err("IOReport GPU energy overflowed".into());
                }
            }
        }

        let timestamp = now_millis();
        let interval_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        let mut metrics = Vec::new();
        if total_ticks > 0 {
            metrics.push(MetricObservation {
                device_id: device_id.into(),
                metric: MetricKey::UtilizationOverall,
                value: MetricValue::Number((active_ticks as f64 / total_ticks as f64) * 100.0),
                source: PROVIDER_ID.into(),
                quality: MetricQuality::Derived,
                sampled_at: timestamp,
                interval_ms: Some(interval_ms),
                definition: Some(
                    "Apple IOReport non-idle GPU state residency over the sampling interval".into(),
                ),
            });
        }
        if self.has_power() && elapsed.as_secs_f64() > 0.0 {
            metrics.push(MetricObservation {
                device_id: device_id.into(),
                metric: MetricKey::PowerEnergyJoules,
                value: MetricValue::Number(energy_joules),
                source: PROVIDER_ID.into(),
                quality: MetricQuality::Derived,
                sampled_at: timestamp,
                interval_ms: Some(interval_ms),
                definition: Some("GPU energy delta reported by Apple IOReport".into()),
            });
            metrics.push(MetricObservation {
                device_id: device_id.into(),
                metric: MetricKey::PowerDrawWatts,
                value: MetricValue::Number(energy_joules / elapsed.as_secs_f64()),
                source: PROVIDER_ID.into(),
                quality: MetricQuality::Derived,
                sampled_at: timestamp,
                interval_ms: Some(interval_ms),
                definition: Some(
                    "GPU energy delta divided by the monotonic sampling interval".into(),
                ),
            });
        }

        Ok(ProviderSample {
            metrics,
            ..ProviderSample::default()
        })
    }

    fn clear_baseline(&mut self) {
        if let Some((sample, _)) = self.baseline.take() {
            // SAFETY: sample is retained by CreateSamples.
            unsafe { CFRelease(sample.cast()) };
        }
    }
}

impl Drop for IoReport {
    fn drop(&mut self) {
        self.clear_baseline();
        // SAFETY: every object is retained and released exactly once here.
        unsafe {
            CFRelease(self.selected_channels.cast());
            CFRelease(self.channels.cast());
            CFRelease(self.source_channels.cast());
            CFRelease(self.subscription.cast());
        }
    }
}

enum ProviderState {
    Available(IoReport),
    Unavailable(String),
}

pub struct AppleIoReportProvider {
    state: Mutex<ProviderState>,
}

impl AppleIoReportProvider {
    pub fn new() -> Self {
        let state = IoReport::open()
            .map(ProviderState::Available)
            .unwrap_or_else(ProviderState::Unavailable);
        Self {
            state: Mutex::new(state),
        }
    }
}

impl InventoryProvider for AppleIoReportProvider {
    fn provider_id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn enumerate(&self) -> Result<Vec<DeviceObservation>> {
        let state = self.state.lock();
        let ProviderState::Available(report) = &*state else {
            return Ok(Vec::new());
        };
        let name = metal::Device::system_default()
            .map(|device| device.name().to_owned())
            .unwrap_or_else(|| "Apple GPU".into());
        let mut observation =
            DeviceObservation::new(PROVIDER_ID, "apple-gpu", GpuVendor::Apple, name);
        observation.kind = GpuKind::Integrated;
        observation.enumeration_ordinal = Some(0);
        observation.identity_priority = 60;
        observation.capabilities = CapabilitySet::new(report.capability_keys())
            .with_extension("apple.activeResidency")
            .with_extension("apple.privateIoReport");
        Ok(vec![observation])
    }

    fn diagnostic(&self) -> ProviderDiagnostic {
        match &*self.state.lock() {
            ProviderState::Available(_) => ProviderDiagnostic {
                id: PROVIDER_ID.into(),
                loaded: true,
                version: macos_major().map(|major| major.to_string()),
                devices_matched: 0,
                reason: None,
                message: Some("private API; dynamically loaded and version-gated".into()),
            },
            ProviderState::Unavailable(message) => ProviderDiagnostic {
                id: PROVIDER_ID.into(),
                loaded: false,
                version: macos_major().map(|major| major.to_string()),
                devices_matched: 0,
                reason: Some(UnavailableReason::DriverLibraryMissing),
                message: Some(message.clone()),
            },
        }
    }
}

impl TelemetryProvider for AppleIoReportProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata::new(PROVIDER_ID, 100, 75)
            .prefer(MetricKey::UtilizationOverall, 100)
            .prefer(MetricKey::PowerDrawWatts, 100)
    }

    fn capabilities(&self, device: &CanonicalGpu) -> CapabilitySet {
        if device.identity.vendor != GpuVendor::Apple
            || !device.provider_device_ids.contains_key(PROVIDER_ID)
        {
            return CapabilitySet::default();
        }
        match &*self.state.lock() {
            ProviderState::Available(report) => CapabilitySet::new(report.capability_keys())
                .with_extension("apple.activeResidency")
                .with_extension("apple.privateIoReport"),
            ProviderState::Unavailable(_) => CapabilitySet::default(),
        }
    }

    fn sample(&self, device: &CanonicalGpu, _request: &SampleRequest) -> Result<ProviderSample> {
        if device.identity.vendor != GpuVendor::Apple
            || !device.provider_device_ids.contains_key(PROVIDER_ID)
        {
            return Ok(ProviderSample::default());
        }
        match &mut *self.state.lock() {
            ProviderState::Available(report) => {
                report.take_sample(&device.identity.id).map_err(|message| {
                    GpuError::provider(
                        PROVIDER_ID,
                        UnavailableReason::TemporarilyUnavailable,
                        message,
                    )
                })
            }
            ProviderState::Unavailable(message) => Err(GpuError::provider(
                PROVIDER_ID,
                UnavailableReason::DriverLibraryMissing,
                message.clone(),
            )),
        }
    }
}

fn is_gpu_channel(channel: &Channel) -> bool {
    (channel.group == "GPU Stats" && channel.subgroup == "GPU Performance States")
        || (channel.group == "Energy Model" && channel.name == "GPU Energy")
}

fn is_active_state(name: &str) -> bool {
    let normalized = name.trim().to_ascii_uppercase();
    !matches!(normalized.as_str(), "OFF" | "IDLE" | "DOWN")
}

fn energy_to_joules(value: f64, unit: &str) -> std::result::Result<f64, String> {
    match unit.trim() {
        "mJ" => Ok(value / 1_000.0),
        "uJ" | "µJ" => Ok(value / 1_000_000.0),
        "nJ" => Ok(value / 1_000_000_000.0),
        other => Err(format!("unsupported IOReport energy unit {other}")),
    }
}

fn dictionary_value(dictionary: CFDictionaryRef, key: &str) -> Option<CFTypeRef> {
    if !cf_type_is(dictionary.cast(), unsafe { CFDictionaryGetTypeID() }) {
        return None;
    }
    let key = cf_create_string(key)?;
    // SAFETY: dictionary and key are valid CF objects for this call.
    let value = unsafe { CFDictionaryGetValue(dictionary, key.cast()) };
    // SAFETY: key is retained by CFStringCreateWithBytes.
    unsafe { CFRelease(key.cast()) };
    (!value.is_null()).then_some(value)
}

fn set_dictionary_value(dictionary: CFMutableDictionaryRef, key: &str, value: CFTypeRef) {
    let Some(key) = cf_create_string(key) else {
        return;
    };
    // SAFETY: dictionary, key, and value are valid CF objects.
    unsafe {
        CFDictionarySetValue(dictionary, key.cast(), value);
        CFRelease(key.cast());
    }
}

fn cf_create_string(value: &str) -> Option<CFStringRef> {
    // SAFETY: input bytes remain valid for the call; the null allocator tells
    // CoreFoundation not to deallocate Rust-owned bytes.
    let result = unsafe {
        CFStringCreateWithBytes(
            kCFAllocatorDefault,
            value.as_ptr(),
            CFIndex::try_from(value.len()).ok()?,
            kCFStringEncodingUTF8,
            0,
        )
    };
    (!result.is_null()).then_some(result)
}

fn cf_string(value: CFStringRef) -> String {
    if !cf_type_is(value.cast(), unsafe { CFStringGetTypeID() }) {
        return String::new();
    }
    // SAFETY: value is a CFString returned by IOReport.
    let length = unsafe { CFStringGetLength(value) };
    let Ok(length_usize) = usize::try_from(length) else {
        return String::new();
    };
    if length_usize > MAX_CF_STRING_UNITS {
        return String::new();
    }
    // SAFETY: encoding is valid and length came from the same string.
    let capacity = unsafe { CFStringGetMaximumSizeForEncoding(length, kCFStringEncodingUTF8) }
        .saturating_add(1);
    let Ok(capacity_usize) = usize::try_from(capacity) else {
        return String::new();
    };
    if capacity_usize == 0 || capacity_usize > MAX_CF_STRING_BYTES {
        return String::new();
    }
    let mut buffer = vec![0_i8; capacity_usize];
    // SAFETY: buffer is writable for capacity bytes and value is a CFString.
    let converted =
        unsafe { CFStringGetCString(value, buffer.as_mut_ptr(), capacity, kCFStringEncodingUTF8) };
    if converted == 0 {
        return String::new();
    }
    // SAFETY: CFStringGetCString wrote a terminating nul into the buffer.
    unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

fn macos_major() -> Option<u32> {
    let name = c"kern.osproductversion";
    let mut size = 0_usize;
    // SAFETY: this first call asks only for the required output size.
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            ptr::null_mut(),
            &raw mut size,
            ptr::null_mut(),
            0,
        )
    } != 0
        || size == 0
        || size > MAX_SYSCTL_VERSION_BYTES
    {
        return None;
    }
    let mut bytes = vec![0_u8; size];
    // SAFETY: bytes has the size returned by the first call.
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            &raw mut size,
            ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    bytes.truncate(size);
    let text = std::ffi::CStr::from_bytes_until_nul(&bytes)
        .ok()?
        .to_str()
        .ok()?;
    text.split('.').next()?.parse().ok()
}

fn cf_type_is(value: CFTypeRef, expected: CFTypeID) -> bool {
    // SAFETY: callers only pass non-owning references returned by
    // CoreFoundation/IOReport; null is rejected before querying the type ID.
    !value.is_null() && unsafe { CFGetTypeID(value) == expected }
}

fn checked_cf_count(
    label: &str,
    count: CFIndex,
    maximum: usize,
) -> std::result::Result<CFIndex, String> {
    let count_usize = usize::try_from(count)
        .map_err(|_| format!("{label} returned a negative or oversized count"))?;
    if count_usize > maximum {
        return Err(format!(
            "{label} count {count_usize} exceeds the {maximum}-entry safety limit"
        ));
    }
    Ok(count)
}

fn checked_i32_count(
    label: &str,
    count: i32,
    maximum: usize,
) -> std::result::Result<usize, String> {
    let count = usize::try_from(count).map_err(|_| format!("{label} returned a negative count"))?;
    if count > maximum {
        return Err(format!(
            "{label} count {count} exceeds the {maximum}-entry safety limit"
        ));
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_only_non_idle_states_as_active() {
        assert!(!is_active_state("OFF"));
        assert!(!is_active_state(" idle "));
        assert!(is_active_state("P3"));
    }

    #[test]
    fn converts_documented_energy_units() {
        assert_eq!(energy_to_joules(1_000.0, "mJ").unwrap(), 1.0);
        assert_eq!(energy_to_joules(1_000_000.0, "uJ").unwrap(), 1.0);
        assert!(energy_to_joules(1.0, "ticks").is_err());
    }

    #[test]
    fn rejects_negative_and_oversized_private_api_counts() {
        assert!(checked_cf_count("fixture", -1, 10).is_err());
        assert!(checked_cf_count("fixture", 11, 10).is_err());
        assert_eq!(checked_cf_count("fixture", 10, 10), Ok(10));
        assert_eq!(
            checked_cf_count(
                "modern Apple Silicon fixture",
                11_884,
                MAX_IOREPORT_CHANNELS,
            ),
            Ok(11_884)
        );
        assert!(
            checked_cf_count(
                "oversized Apple Silicon fixture",
                65_537,
                MAX_IOREPORT_CHANNELS,
            )
            .is_err()
        );
        assert!(checked_i32_count("fixture", -1, 10).is_err());
        assert!(checked_i32_count("fixture", 11, 10).is_err());
        assert_eq!(checked_i32_count("fixture", 10, 10), Ok(10));
    }

    #[test]
    fn provider_initialization_never_panics() {
        let provider = AppleIoReportProvider::new();
        assert_eq!(provider.provider_id(), PROVIDER_ID);
    }
}
