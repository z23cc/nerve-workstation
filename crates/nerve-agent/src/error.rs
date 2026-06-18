//! Error types shared across the agent crate.

use thiserror::Error;

/// Errors surfaced by the agent crate (providers, auth, orchestration, tools).
#[derive(Debug, Error)]
pub enum AgentError {
    /// A transport-level HTTP failure.
    #[error("http error: {0}")]
    Http(String),
    /// A provider returned an error or unexpected payload.
    #[error("provider error: {0}")]
    Provider(String),
    /// Authentication/credential failure.
    #[error("auth error: {0}")]
    Auth(String),
    /// Failed to parse a response or payload.
    #[error("parse error: {0}")]
    Parse(String),
    /// A tool invocation failed.
    #[error("tool error: {0}")]
    Tool(String),
    /// Configuration was invalid or missing.
    #[error("config error: {0}")]
    Config(String),
    /// The operation was cancelled via a `CancelToken`.
    #[error("cancelled")]
    Cancelled,
    /// A stubbed code path that has not been implemented yet.
    #[error("unimplemented")]
    Unimplemented,
}

impl AgentError {
    /// Construct a generic [`AgentError::Provider`] from any string-like message.
    pub fn msg(s: impl Into<String>) -> Self {
        AgentError::Provider(s.into())
    }
}

/// Convenience result alias for the agent crate.
pub type AgentResult<T> = Result<T, AgentError>;
