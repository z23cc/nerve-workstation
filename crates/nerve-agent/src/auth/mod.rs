//! Authentication contract: providers, credentials, and login strategies.
//!
//! The concrete persistence (`store`), OAuth flows (`oauth`), and per-provider
//! strategies (`strategy`) are filled in during the Implement phase. This module
//! defines the shared types and the entry points other code links against.

use std::sync::{Mutex, MutexGuard};

use crate::error::{AgentError, AgentResult};
use nerve_core::CancelToken;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// Cooperative cancellation for the login flow.
    pub cancel: nerve_core::CancelToken,
}

impl Default for LoginOptions {
    fn default() -> Self {
        Self {
            no_browser: false,
            manual_paste: false,
            timeout: std::time::Duration::from_secs(300),
            cancel: nerve_core::CancelToken::never(),
        }
    }
}

/// Serializable state for an OAuth authorization-code login that has been
/// started but not yet completed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginStart {
    pub provider: ProviderId,
    pub authorize_url: String,
    pub redirect_uri: String,
    pub state: String,
    pub verifier: String,
    #[serde(default)]
    pub provider_data: Value,
}

/// A per-provider login/refresh strategy.
pub trait AuthStrategy: Send + Sync {
    /// The provider this strategy authenticates.
    fn provider(&self) -> ProviderId;
    /// Default redirect URI for non-listening, protocol-driven login flows.
    fn default_redirect_uri(&self) -> String;
    /// Start an OAuth authorization-code flow for `redirect_uri`.
    fn start(&self, redirect_uri: &str) -> AgentResult<LoginStart>;
    /// Complete a started OAuth authorization-code flow.
    fn complete(
        &self,
        start: &LoginStart,
        callback: &oauth::OAuthCallback,
        cancel: &CancelToken,
    ) -> AgentResult<Credential>;
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

/// Return `credential`, refreshing and persisting it first when an OAuth access
/// token has expired (or when `force_refresh` asks for a refresh). Refreshes are
/// single-flight per provider; waiters re-load the store after acquiring the lock
/// so a rotated refresh token saved by the first refresher is reused instead of
/// refreshed again.
pub fn ensure_fresh(credential: Credential, force_refresh: bool) -> AgentResult<Credential> {
    ensure_fresh_with(
        credential,
        force_refresh,
        load_credential,
        save_credential,
        |cred| strategy_for(cred.provider).refresh(cred),
    )
}

fn ensure_fresh_with(
    credential: Credential,
    force_refresh: bool,
    load: impl Fn(ProviderId) -> AgentResult<Option<Credential>>,
    save: impl Fn(&Credential) -> AgentResult<()>,
    refresh: impl Fn(&Credential) -> AgentResult<Credential>,
) -> AgentResult<Credential> {
    if !should_refresh(&credential, force_refresh) {
        return Ok(credential);
    }

    let provider = credential.provider;
    let _process_guard = refresh_lock(provider)?;
    let _file_guard = store::acquire_refresh_lock(provider)?;
    let latest = load(provider)?.unwrap_or_else(|| credential.clone());
    if double_check_is_fresh(&credential, &latest, force_refresh) {
        return Ok(latest);
    }

    let refreshed = refresh(&latest)?;
    save(&refreshed)?;
    Ok(refreshed)
}

fn should_refresh(credential: &Credential, force_refresh: bool) -> bool {
    matches!(credential.mode, AuthMode::Oauth) && (force_refresh || is_expired(credential))
}

fn double_check_is_fresh(original: &Credential, latest: &Credential, force_refresh: bool) -> bool {
    if !matches!(latest.mode, AuthMode::Oauth) {
        return true;
    }
    if is_expired(latest) {
        return false;
    }
    !force_refresh || !same_credential(original, latest)
}

fn is_expired(credential: &Credential) -> bool {
    credential
        .expires_at_unix
        .is_some_and(|exp| oauth::now_unix() >= exp)
}

fn same_credential(left: &Credential, right: &Credential) -> bool {
    left.provider == right.provider
        && left.mode == right.mode
        && left.access_token == right.access_token
        && left.refresh_token == right.refresh_token
        && left.expires_at_unix == right.expires_at_unix
        && left.account_id == right.account_id
        && left.base_url == right.base_url
}

static ANTHROPIC_REFRESH_LOCK: Mutex<()> = Mutex::new(());
static OPENAI_REFRESH_LOCK: Mutex<()> = Mutex::new(());
static XAI_REFRESH_LOCK: Mutex<()> = Mutex::new(());

fn refresh_lock(provider: ProviderId) -> AgentResult<MutexGuard<'static, ()>> {
    let lock = match provider {
        ProviderId::Anthropic => &ANTHROPIC_REFRESH_LOCK,
        ProviderId::OpenAi => &OPENAI_REFRESH_LOCK,
        ProviderId::Xai => &XAI_REFRESH_LOCK,
    };
    lock.lock()
        .map_err(|_| AgentError::Auth(format!("{} refresh lock poisoned", provider.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Barrier, Mutex as StdMutex};
    use std::thread;

    fn oauth_credential(
        provider: ProviderId,
        access_token: &str,
        expires_at_unix: u64,
    ) -> Credential {
        Credential {
            provider,
            mode: AuthMode::Oauth,
            access_token: access_token.into(),
            refresh_token: Some(format!("refresh-{access_token}")),
            expires_at_unix: Some(expires_at_unix),
            account_id: Some("acct".into()),
            base_url: provider.default_base_url().to_string(),
        }
    }

    #[test]
    fn ensure_fresh_leaves_api_keys_unchanged() {
        let credential = Credential {
            provider: ProviderId::OpenAi,
            mode: AuthMode::ApiKey,
            access_token: "sk".into(),
            refresh_token: None,
            expires_at_unix: Some(0),
            account_id: None,
            base_url: ProviderId::OpenAi.default_base_url().to_string(),
        };
        let calls = AtomicU32::new(0);
        let out = ensure_fresh_with(
            credential.clone(),
            false,
            |_| Ok(None),
            |_| Ok(()),
            |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                unreachable!("api keys do not refresh")
            },
        )
        .expect("api key ok");
        assert!(same_credential(&credential, &out));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn ensure_fresh_refreshes_and_saves_expired_oauth() {
        let expired = oauth_credential(ProviderId::OpenAi, "old", 0);
        let fresh = oauth_credential(ProviderId::OpenAi, "new", oauth::now_unix() + 3600);
        let saved = Arc::new(StdMutex::new(None));
        let saved_for_save = Arc::clone(&saved);
        let out = ensure_fresh_with(
            expired.clone(),
            false,
            |_| Ok(Some(expired.clone())),
            move |cred| {
                *saved_for_save.lock().expect("saved lock") = Some(cred.clone());
                Ok(())
            },
            |_| Ok(fresh.clone()),
        )
        .expect("refreshed");
        assert_eq!(out.access_token, "new");
        assert_eq!(
            saved
                .lock()
                .expect("saved lock")
                .as_ref()
                .map(|c| c.access_token.as_str()),
            Some("new")
        );
    }

    #[test]
    fn ensure_fresh_single_flight_reloads_after_lock() {
        let expired = oauth_credential(ProviderId::Anthropic, "old", 0);
        let fresh = oauth_credential(ProviderId::Anthropic, "new", oauth::now_unix() + 3600);
        let store = Arc::new(StdMutex::new(expired.clone()));
        let refresh_calls = Arc::new(AtomicU32::new(0));
        let barrier = Arc::new(Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let store_for_load = Arc::clone(&store);
                let store_for_save = Arc::clone(&store);
                let calls = Arc::clone(&refresh_calls);
                let start = Arc::clone(&barrier);
                let input = expired.clone();
                let fresh = fresh.clone();
                thread::spawn(move || {
                    start.wait();
                    ensure_fresh_with(
                        input,
                        false,
                        move |_| Ok(Some(store_for_load.lock().expect("store lock").clone())),
                        move |cred| {
                            *store_for_save.lock().expect("store lock") = cred.clone();
                            Ok(())
                        },
                        move |_| {
                            calls.fetch_add(1, Ordering::SeqCst);
                            Ok(fresh.clone())
                        },
                    )
                    .expect("fresh")
                })
            })
            .collect();

        for handle in handles {
            let credential = handle.join().expect("thread joined");
            assert_eq!(credential.access_token, "new");
        }
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
    }
}
