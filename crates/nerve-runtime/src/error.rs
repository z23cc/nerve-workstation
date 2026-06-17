use nerve_core::dispatch::dispatch_error_kind;
use nerve_core::{DispatchError, NerveError};
use thiserror::Error;

/// Runtime error surfaced by transport adapters.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// Error returned by the built-in context-engine tool dispatcher.
    #[error(transparent)]
    Core(#[from] DispatchError),
    /// Error returned while encoding or decoding runtime JSON values.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Error message returned by a capability adapter.
    #[error("{0}")]
    Adapter(String),
}

impl RuntimeError {
    #[must_use]
    pub fn adapter(error: impl Into<String>) -> Self {
        Self::Adapter(error.into())
    }

    #[must_use]
    pub fn cancelled() -> Self {
        Self::Core(DispatchError::Core(NerveError::Cancelled))
    }

    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Core(err) => dispatch_error_kind(err),
            Self::Json(_) => "json",
            Self::Adapter(_) => "adapter",
        }
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Core(DispatchError::Core(NerveError::Cancelled)))
    }
}
