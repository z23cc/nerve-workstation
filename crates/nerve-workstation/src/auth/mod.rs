//! Credential access for workstation OAuth-backed tools, plus the `nerve auth` CLI.
//!
//! OAuth flows and credential storage are owned by [`nerve_agent::auth`] — the
//! single source of truth for every provider. This module is a thin adapter for
//! provider-specific tool runtime URL needs and hosts the xAI-only `nerve auth`
//! CLI, an alias for `nerve agent login --provider xai` over the same store.

use anyhow::{Result, anyhow};
use nerve_agent::auth::{self, AuthMode, ProviderId};

mod commands;
mod manager;

pub(crate) use commands::AuthArgs;
pub(crate) use manager::AuthManager;

/// Credentials resolved for the xAI (Grok) tool runtime.
pub(crate) struct RuntimeCredentials {
    pub(crate) base_url: String,
    pub(crate) access_token: String,
}

/// Credentials resolved for ChatGPT/Codex subscription tool calls.
pub(crate) struct OpenAiCodexCredentials {
    pub(crate) base_url: String,
    pub(crate) access_token: String,
    pub(crate) account_id: Option<String>,
}

pub(crate) fn run(args: AuthArgs) -> Result<()> {
    commands::run(args)
}

/// Resolve the stored xAI credential for tool calls, refreshing when the token
/// is expiring (or always when `force_refresh`). Fails closed if not logged in.
pub(crate) fn resolve_runtime_credentials(force_refresh: bool) -> Result<RuntimeCredentials> {
    let credential = auth::load_credential(ProviderId::Xai)
        .map_err(|err| anyhow!("failed to load xAI credentials: {err}"))?
        .ok_or_else(|| anyhow!("not logged in to xAI; run `nerve agent login --provider xai`"))?;
    let credential = auth::ensure_fresh(credential, force_refresh)
        .map_err(|err| anyhow!("failed to refresh xAI token: {err}"))?;
    Ok(RuntimeCredentials {
        base_url: inference_base_url(&credential.base_url),
        access_token: credential.access_token,
    })
}

/// Resolve the stored OpenAI OAuth credential for Codex backend tool calls.
/// The stored base URL is already the Codex backend; do not append `/v1`.
pub(crate) fn resolve_openai_codex_credentials(
    force_refresh: bool,
) -> Result<OpenAiCodexCredentials> {
    let credential = auth::load_credential(ProviderId::OpenAi)
        .map_err(|err| anyhow!("failed to load OpenAI credentials: {err}"))?
        .ok_or_else(|| {
            anyhow!("not logged in to ChatGPT/OpenAI; run `nerve agent login --provider chatgpt`")
        })?;
    let credential = auth::ensure_fresh(credential, force_refresh)
        .map_err(|err| anyhow!("failed to refresh OpenAI token: {err}"))?;
    if credential.mode != AuthMode::Oauth {
        return Err(anyhow!(
            "OpenAI Codex tools require ChatGPT OAuth; run `nerve agent login --provider chatgpt`"
        ));
    }
    let base_url = codex_base_url(&credential.base_url);
    Ok(OpenAiCodexCredentials {
        base_url,
        access_token: credential.access_token,
        account_id: credential.account_id,
    })
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

/// Canonical ChatGPT/Codex subscription backend. The endpoint is fixed, so
/// derive it rather than trusting the stored base: credentials minted before the
/// codex base was persisted keep the canonical host (`api.openai.com`), and the
/// chat provider likewise hardcodes this endpoint for OAuth.
const OPENAI_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

fn codex_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/backend-api/codex") {
        trimmed.to_string()
    } else {
        OPENAI_CODEX_BASE_URL.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{codex_base_url, inference_base_url};

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

    #[test]
    fn codex_base_derives_canonical_backend() {
        assert_eq!(
            codex_base_url("https://chatgpt.com/backend-api/codex/"),
            "https://chatgpt.com/backend-api/codex"
        );
        // A stale/canonical host (pre-codex-base login) derives the fixed backend.
        assert_eq!(
            codex_base_url("https://api.openai.com"),
            "https://chatgpt.com/backend-api/codex"
        );
    }
}
