//! xAI credential access for the Grok tools, plus the `nerve auth` CLI.
//!
//! OAuth flows and credential storage are owned by [`nerve_agent::auth`] — the
//! single source of truth for every provider. This module is a thin adapter:
//! it resolves the stored xAI credential for the Grok tool runtime (whose tools
//! build URLs against a `/v1` base) and hosts the xAI-only `nerve auth` CLI, an
//! alias for `nerve agent login --provider xai` over the same store.

use anyhow::{Result, anyhow};
use nerve_agent::auth::{self, AuthMode, ProviderId};

mod commands;

pub(crate) use commands::AuthArgs;

/// Credentials resolved for the xAI (Grok) tool runtime.
pub(crate) struct RuntimeCredentials {
    pub(crate) base_url: String,
    pub(crate) access_token: String,
}

/// Refresh the token this many seconds before it expires.
const REFRESH_SKEW_SECONDS: u64 = 3_600;

pub(crate) fn run(args: AuthArgs) -> Result<()> {
    commands::run(args)
}

/// Resolve the stored xAI credential for tool calls, refreshing when the token
/// is expiring (or always when `force_refresh`). Fails closed if not logged in.
pub(crate) fn resolve_runtime_credentials(force_refresh: bool) -> Result<RuntimeCredentials> {
    let credential = auth::load_credential(ProviderId::Xai)
        .map_err(|err| anyhow!("failed to load xAI credentials: {err}"))?
        .ok_or_else(|| anyhow!("not logged in to xAI; run `nerve agent login --provider xai`"))?;
    let credential = if force_refresh || is_expiring(&credential) {
        refresh(credential)?
    } else {
        credential
    };
    Ok(RuntimeCredentials {
        base_url: inference_base_url(&credential.base_url),
        access_token: credential.access_token,
    })
}

fn refresh(credential: auth::Credential) -> Result<auth::Credential> {
    // API keys never refresh.
    if matches!(credential.mode, AuthMode::ApiKey) {
        return Ok(credential);
    }
    let refreshed = auth::strategy_for(ProviderId::Xai)
        .refresh(&credential)
        .map_err(|err| anyhow!("failed to refresh xAI token: {err}"))?;
    auth::save_credential(&refreshed)
        .map_err(|err| anyhow!("failed to persist refreshed xAI token: {err}"))?;
    Ok(refreshed)
}

fn is_expiring(credential: &auth::Credential) -> bool {
    match credential.expires_at_unix {
        Some(exp) => now_unix().saturating_add(REFRESH_SKEW_SECONDS) >= exp,
        None => false,
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// The Grok tools build URLs as `{base}/responses`, `{base}/models`, etc., so
/// the base must carry the `/v1` segment. The stored credential keeps the
/// canonical host (`https://api.x.ai`); append `/v1` when it is absent.
fn inference_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1")
    }
}

#[cfg(test)]
mod tests {
    use super::inference_base_url;

    #[test]
    fn appends_v1_when_missing() {
        assert_eq!(
            inference_base_url("https://api.x.ai"),
            "https://api.x.ai/v1"
        );
        assert_eq!(
            inference_base_url("https://api.x.ai/"),
            "https://api.x.ai/v1"
        );
        assert_eq!(
            inference_base_url("https://api.x.ai/v1"),
            "https://api.x.ai/v1"
        );
        assert_eq!(
            inference_base_url("https://api.x.ai/v1/"),
            "https://api.x.ai/v1"
        );
    }
}
