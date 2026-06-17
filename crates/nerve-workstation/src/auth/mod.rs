use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    thread::sleep,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const PROVIDER_ID: &str = "xai-oauth";
const DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";
const DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const REDIRECT_HOST: &str = "127.0.0.1";
const REDIRECT_PORT: u16 = 56_121;
const REDIRECT_PATH: &str = "/callback";
const REFRESH_SKEW_SECONDS: u64 = 3_600;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct AuthStore {
    #[serde(default)]
    providers: BTreeMap<String, XaiProviderState>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct XaiProviderState {
    #[serde(default)]
    tokens: Option<XaiTokens>,
    #[serde(default)]
    discovery: Option<XaiDiscovery>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    auth_mode: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    last_refresh_unix: Option<u64>,
    #[serde(default)]
    last_auth_error: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct XaiTokens {
    access_token: String,
    refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_in: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct XaiDiscovery {
    authorization_endpoint: String,
    token_endpoint: String,
}

#[derive(Debug)]
pub(crate) struct RuntimeCredentials {
    pub(crate) base_url: String,
    pub(crate) access_token: String,
    pub(crate) last_refresh_unix: Option<u64>,
}

mod callback;
mod commands;
mod http;
mod oauth;
mod store;
mod util;

pub(crate) use commands::AuthArgs;

use oauth::refresh_tokens;
use store::{acquire_auth_lock, auth_file_path, load_store, save_store, xai_state_and_tokens};
use util::{access_token_is_expiring, now_unix, validate_inference_base_url};

pub(crate) fn run(args: AuthArgs) -> Result<()> {
    commands::run(args)
}

pub(crate) fn resolve_runtime_credentials(force_refresh: bool) -> Result<RuntimeCredentials> {
    let path = auth_file_path()?;
    let store = load_store(&path)?;
    let (mut state, mut tokens) = xai_state_and_tokens(&store)?;
    let needs_refresh =
        force_refresh || access_token_is_expiring(&tokens.access_token, REFRESH_SKEW_SECONDS);
    if needs_refresh {
        let _lock = acquire_auth_lock(&path)?;
        let mut store = load_store(&path)?;
        (state, tokens) = xai_state_and_tokens(&store)?;
        if force_refresh || access_token_is_expiring(&tokens.access_token, REFRESH_SKEW_SECONDS) {
            tokens = refresh_tokens(&state, &tokens)?;
            state.tokens = Some(tokens.clone());
            state.last_refresh_unix = Some(now_unix());
            state.last_auth_error = None;
            store
                .providers
                .insert(PROVIDER_ID.to_string(), state.clone());
            save_store(&path, &store)?;
        }
    }
    let base_url = validate_inference_base_url(state.base_url.as_deref())?;
    Ok(RuntimeCredentials {
        base_url,
        access_token: tokens.access_token,
        last_refresh_unix: state.last_refresh_unix,
    })
}

#[cfg(test)]
mod tests {
    use super::callback::parse_callback_target;
    use super::util::{jwt_expiry, pkce_challenge, validate_oauth_endpoint};
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc_example() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn query_parser_decodes_callback_values() {
        let callback = parse_callback_target(
            "/callback?code=a%2Fb%2Bc&state=hello+world&error_description=nope",
            false,
        )
        .expect("callback");
        assert_eq!(callback.code.as_deref(), Some("a/b+c"));
        assert_eq!(callback.state.as_deref(), Some("hello world"));
        assert_eq!(callback.error_description.as_deref(), Some("nope"));
    }

    #[test]
    fn validates_xai_hosts_only() {
        validate_oauth_endpoint("https://auth.x.ai/token", "token_endpoint").expect("xai host");
        validate_oauth_endpoint("https://accounts.x.ai/oauth", "authorization_endpoint")
            .expect("xai subdomain");
        assert!(validate_oauth_endpoint("https://example.com/token", "token_endpoint").is_err());
        assert!(
            validate_oauth_endpoint("https://attacker.example?@api.x.ai/v1", "token_endpoint")
                .is_err()
        );
        assert!(validate_inference_base_url(Some("https://api.x.ai/v1")).is_ok());
        assert!(validate_inference_base_url(Some("https://staging.x.ai/v1")).is_ok());
        assert!(validate_inference_base_url(Some("https://x.ai/v1")).is_err());
        assert!(validate_inference_base_url(Some("http://api.x.ai/v1")).is_err());
        assert!(validate_inference_base_url(Some("https://api.x.ai/v1?token=leak")).is_err());
    }

    #[test]
    fn jwt_expiry_reads_exp_claim() {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(br#"{"exp":4102444800}"#);
        let token = format!("{header}.{payload}.sig");
        assert_eq!(jwt_expiry(&token), Some(4_102_444_800));
    }

    #[test]
    fn keyring_accounts_are_scoped_by_auth_file_path() {
        let first = store::keyring_account_for_path(Path::new("/tmp/a/auth.json"));
        let second = store::keyring_account_for_path(Path::new("/tmp/b/auth.json"));
        assert_ne!(first, second);
        assert!(first.starts_with("xai-oauth:"));
    }

    #[test]
    fn save_and_load_store_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        let mut store = AuthStore::default();
        store.providers.insert(
            PROVIDER_ID.to_string(),
            XaiProviderState {
                tokens: Some(XaiTokens {
                    access_token: "access".to_string(),
                    refresh_token: "refresh".to_string(),
                    id_token: None,
                    expires_in: Some(3600),
                    token_type: Some("Bearer".to_string()),
                }),
                base_url: Some(DEFAULT_BASE_URL.to_string()),
                ..XaiProviderState::default()
            },
        );
        save_store(&path, &store).expect("save");
        let loaded = load_store(&path).expect("load");
        assert!(loaded.providers.contains_key(PROVIDER_ID));
    }
}
