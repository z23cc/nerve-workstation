//! Authentication contract: providers, credentials, and login strategies.
//!
//! The concrete persistence (`store`), OAuth flows (`oauth`), and per-provider
//! strategies (`strategy`) are filled in during the Implement phase. This module
//! defines the shared types and the entry points other code links against.

use crate::error::AgentResult;
use serde::{Deserialize, Serialize};

pub mod oauth;
pub mod store;
pub mod strategy;

pub use store::config_home;
pub use strategy::{from_api_key, strategy_for};

/// The set of LLM providers this crate can talk to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    Anthropic,
    OpenAi,
    Xai,
}

impl ProviderId {
    /// Stable lowercase identifier used in config and storage keys.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderId::Anthropic => "anthropic",
            ProviderId::OpenAi => "openai",
            ProviderId::Xai => "xai",
        }
    }

    /// Default API base URL for the provider.
    pub fn default_base_url(self) -> &'static str {
        match self {
            ProviderId::Anthropic => "https://api.anthropic.com",
            ProviderId::OpenAi => "https://api.openai.com",
            ProviderId::Xai => "https://api.x.ai",
        }
    }
}

/// How a credential authenticates against a provider.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    ApiKey,
    Oauth,
}

/// A resolved credential for a single provider.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credential {
    /// Which provider this credential authenticates against.
    pub provider: ProviderId,
    /// Whether this is an API key or an OAuth token.
    pub mode: AuthMode,
    /// The bearer token / API key used in the `Authorization` header.
    pub access_token: String,
    /// OAuth refresh token, if applicable.
    pub refresh_token: Option<String>,
    /// Unix timestamp (seconds) at which `access_token` expires, if known.
    pub expires_at_unix: Option<u64>,
    /// Provider account identifier, if known.
    pub account_id: Option<String>,
    /// API base URL to use for this credential.
    pub base_url: String,
}

/// Options controlling an interactive login flow.
#[derive(Clone, Debug)]
pub struct LoginOptions {
    /// Do not attempt to open a system browser.
    pub no_browser: bool,
    /// Use manual copy/paste of the callback URL instead of a loopback server.
    pub manual_paste: bool,
    /// Overall timeout for the login flow.
    pub timeout: std::time::Duration,
}

impl Default for LoginOptions {
    fn default() -> Self {
        Self {
            no_browser: false,
            manual_paste: false,
            timeout: std::time::Duration::from_secs(300),
        }
    }
}

/// A per-provider login/refresh strategy.
pub trait AuthStrategy: Send + Sync {
    /// The provider this strategy authenticates.
    fn provider(&self) -> ProviderId;
    /// Perform an interactive login, returning a fresh credential.
    fn login(&self, opts: &LoginOptions) -> AgentResult<Credential>;
    /// Refresh an existing credential.
    fn refresh(&self, cred: &Credential) -> AgentResult<Credential>;
}

/// Persist a credential to the platform credential store.
pub fn save_credential(cred: &Credential) -> AgentResult<()> {
    store::save_credential(cred)
}

/// Load a previously saved credential for `provider`, if any.
pub fn load_credential(provider: ProviderId) -> AgentResult<Option<Credential>> {
    store::load_credential(provider)
}

/// Remove a previously saved credential for `provider`, if any.
pub fn delete_credential(provider: ProviderId) -> AgentResult<()> {
    store::delete_credential(provider)
}
