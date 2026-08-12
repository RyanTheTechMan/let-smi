#![deny(unsafe_op_in_unsafe_fn)]

use let_smi_core::sampler::{SampleSubscription, WatchOptions};
use let_smi_core::{GpuError, GpuMonitor};
use napi::bindgen_prelude::{ToNapiValue, TypeName};
use napi::{Error, Status, ValueType};
use napi_derive::napi;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

#[napi]
pub struct NativeMonitor {
    monitor: GpuMonitor,
}

impl Drop for NativeMonitor {
    fn drop(&mut self) {
        self.monitor.request_close_nonblocking();
    }
}

#[napi]
impl NativeMonitor {
    #[napi(js_name = "listGpus")]
    pub fn list_gpus(&self) -> napi::Result<Value> {
        self.monitor
            .gpus()
            .and_then(to_js_value)
            .map_err(to_napi_error)
    }

    #[napi(js_name = "sampleGpu")]
    pub async fn sample_gpu(
        &self,
        device_id: String,
        options: Option<Value>,
    ) -> napi::Result<JsonValue> {
        let request = decode_options(options)?;
        let monitor = self.monitor.clone();
        let output =
            napi::bindgen_prelude::spawn_blocking(move || monitor.sample(device_id, request))
                .await
                .map_err(to_join_error)?
                .map_err(to_napi_error)?;
        to_js_value(output).map(JsonValue).map_err(to_napi_error)
    }

    #[napi(js_name = "subscribeGpu")]
    pub fn subscribe_gpu(
        &self,
        device_id: String,
        options: Option<Value>,
    ) -> napi::Result<NativeSubscription> {
        let options: NativeWatchOptions = decode_options(options)?;
        let subscription = self
            .monitor
            .samples(device_id, options.into())
            .map_err(to_napi_error)?;
        Ok(NativeSubscription {
            subscription: Arc::new(subscription),
        })
    }

    #[napi(js_name = "vendorInfo")]
    pub async fn vendor_info(&self, device_id: String) -> napi::Result<JsonValue> {
        let monitor = self.monitor.clone();
        napi::bindgen_prelude::spawn_blocking(move || monitor.vendor_info(&device_id))
            .await
            .map_err(to_join_error)?
            .map(normalize_js_numbers)
            .map(JsonValue)
            .map_err(to_napi_error)
    }

    #[napi]
    pub async fn diagnostics(&self) -> napi::Result<JsonValue> {
        let monitor = self.monitor.clone();
        let output = napi::bindgen_prelude::spawn_blocking(move || monitor.diagnostics())
            .await
            .map_err(to_join_error)?;
        to_js_value(output).map(JsonValue).map_err(to_napi_error)
    }

    #[napi]
    pub async fn refresh(&self) -> napi::Result<JsonValue> {
        let monitor = self.monitor.clone();
        let output = napi::bindgen_prelude::spawn_blocking(move || monitor.refresh())
            .await
            .map_err(to_join_error)?
            .map_err(to_napi_error)?;
        to_js_value(output).map(JsonValue).map_err(to_napi_error)
    }

    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        let monitor = self.monitor.clone();
        napi::bindgen_prelude::spawn_blocking(move || monitor.close())
            .await
            .map_err(to_join_error)
    }
}

#[napi]
pub struct NativeSubscription {
    subscription: Arc<SampleSubscription>,
}

#[napi]
impl NativeSubscription {
    #[napi]
    pub async fn next(&self) -> napi::Result<Option<JsonValue>> {
        let future = self.subscription.next_async().map_err(to_napi_error)?;
        future
            .await
            .map_err(to_napi_error)?
            .map(|output| to_js_value(output).map(JsonValue))
            .transpose()
            .map_err(to_napi_error)
    }

    #[napi]
    pub fn cancel(&self) {
        self.subscription.cancel();
    }
}

impl Drop for NativeSubscription {
    fn drop(&mut self) {
        self.subscription.cancel();
    }
}

#[napi(js_name = "openMonitor")]
pub async fn open_monitor(options: Option<Value>) -> napi::Result<NativeMonitor> {
    let options = decode_options(options)?;
    let monitor = napi::bindgen_prelude::spawn_blocking(move || GpuMonitor::open(options))
        .await
        .map_err(to_join_error)?
        .map_err(to_napi_error)?;
    Ok(NativeMonitor { monitor })
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NativeWatchOptions {
    #[serde(default = "default_interval_ms")]
    interval_ms: u64,
    #[serde(default)]
    include_processes: bool,
}

impl From<NativeWatchOptions> for WatchOptions {
    fn from(value: NativeWatchOptions) -> Self {
        Self {
            interval_ms: value.interval_ms,
            include_processes: value.include_processes,
        }
    }
}

const fn default_interval_ms() -> u64 {
    1_000
}

pub struct JsonValue(Value);

impl TypeName for JsonValue {
    fn type_name() -> &'static str {
        "unknown"
    }

    fn value_type() -> ValueType {
        ValueType::Unknown
    }
}

impl ToNapiValue for JsonValue {
    unsafe fn to_napi_value(
        env: napi::sys::napi_env,
        value: Self,
    ) -> napi::Result<napi::sys::napi_value> {
        // SAFETY: the environment is provided by NAPI-RS and serde_json::Value
        // owns every nested value for the duration of conversion.
        unsafe { Value::to_napi_value(env, value.0) }
    }
}

fn decode_options<T>(value: Option<Value>) -> napi::Result<T>
where
    T: for<'de> Deserialize<'de> + Default,
{
    value.map_or_else(
        || Ok(T::default()),
        |value| {
            serde_json::from_value(value).map_err(|error| {
                Error::new(
                    Status::InvalidArg,
                    format!("invalid GPU monitor options: {error}"),
                )
            })
        },
    )
}

fn to_js_value<T: Serialize>(value: T) -> let_smi_core::Result<Value> {
    serde_json::to_value(value)
        .map(normalize_js_numbers)
        .map_err(|error| GpuError::Internal(error.to_string()))
}

fn normalize_js_numbers(mut value: Value) -> Value {
    match &mut value {
        Value::Array(values) => {
            for value in values {
                let current = std::mem::take(value);
                *value = normalize_js_numbers(current);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                let current = std::mem::take(value);
                *value = normalize_js_numbers(current);
            }
        }
        Value::Number(number) => {
            if let Some(integer) = number.as_u64()
                && integer > u64::from(u32::MAX)
                && integer <= 9_007_199_254_740_991
                && let Some(float) = serde_json::Number::from_f64(integer as f64)
            {
                *number = float;
            } else if let Some(integer) = number.as_i64()
                && integer.unsigned_abs() > u64::from(u32::MAX)
                && integer.unsigned_abs() <= 9_007_199_254_740_991
                && let Some(float) = serde_json::Number::from_f64(integer as f64)
            {
                *number = float;
            }
        }
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
    value
}

fn to_napi_error(error: GpuError) -> Error {
    let status = match error {
        GpuError::InvalidArgument(_) | GpuError::DeviceNotFound(_) => Status::InvalidArg,
        GpuError::MonitorClosed | GpuError::StreamClosed => Status::Cancelled,
        GpuError::Backpressure(_) | GpuError::Provider { .. } | GpuError::Internal(_) => {
            Status::GenericFailure
        }
    };
    Error::new(status, error.to_string())
}

fn to_join_error(error: impl std::fmt::Display) -> Error {
    Error::new(Status::GenericFailure, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_large_safe_integers_to_javascript_numbers() {
        let value = normalize_js_numbers(serde_json::json!({
            "sampledAt": 1_700_000_000_000_u64,
            "bytes": 24_000_000_000_u64
        }));
        assert!(value["sampledAt"].as_f64().is_some());
        assert!(value["bytes"].as_f64().is_some());
    }

    #[test]
    fn native_option_decoding_rejects_unknown_fields() {
        let error = decode_options::<NativeWatchOptions>(Some(serde_json::json!({
            "intervalMs": 1000,
            "typo": true
        })))
        .expect_err("unknown native options must be rejected");
        assert_eq!(error.status, Status::InvalidArg);
    }
}
