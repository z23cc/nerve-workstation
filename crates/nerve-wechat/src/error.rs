//! Error type for the WeChat gateway client.

use thiserror::Error;

/// A failure talking to the iLink Bot gateway.
#[derive(Debug, Error)]
pub enum WeixinError {
    /// Transport / HTTP-status failure (network, non-2xx, timeout).
    #[error("weixin transport error: {0}")]
    Transport(String),
    /// The gateway returned a non-zero `ret` code (e.g. -14 session timeout).
    #[error("weixin gateway returned ret={ret}")]
    Gateway { ret: i32 },
    /// The response body was not the expected JSON shape.
    #[error("weixin response parse error: {0}")]
    Parse(String),
    /// QR login did not complete (expired, blocked, or timed out).
    #[error("weixin login failed: {0}")]
    Login(String),
}

/// Result alias for gateway operations.
pub type WeixinResult<T> = Result<T, WeixinError>;
