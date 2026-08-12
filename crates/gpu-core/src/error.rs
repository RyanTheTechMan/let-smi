use crate::model::UnavailableReason;
use thiserror::Error;

pub type Result<T, E = GpuError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum GpuError {
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("monitor is closed")]
    MonitorClosed,

    #[error("GPU `{0}` was not found")]
    DeviceNotFound(String),

    #[error("sample stream is closed")]
    StreamClosed,

    #[error("provider `{provider}` failed ({reason:?}): {message}")]
    Provider {
        provider: String,
        reason: UnavailableReason,
        message: String,
    },

    #[error("internal invariant failed: {0}")]
    Internal(String),
}

impl GpuError {
    pub fn provider(
        provider: impl Into<String>,
        reason: UnavailableReason,
        message: impl Into<String>,
    ) -> Self {
        Self::Provider {
            provider: provider.into(),
            reason,
            message: message.into(),
        }
    }
}
