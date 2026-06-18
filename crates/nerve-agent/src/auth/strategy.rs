//! Per-provider login/refresh strategies.
//!
//! Each strategy runs the authorization-code + PKCE flow over a loopback
//! redirect (with a manual-paste fallback) and exchanges the code for a
//! [`Credential`] in `Oauth` mode, then refreshes via the stored refresh token.
//! Endpoints, client ids, and scopes are ported from the oh-my-pi OAuth
//! registry. Request-time impersonation headers are *not* set here — those
//! belong to the provider request path; this module only obtains, refreshes,
//! and shapes tokens.

use std::time::Duration;

use serde_json::{Value, json};

use super::oauth::{
    self, Pkce, announce_and_open, optional_str, post_token_form, post_token_json, required_str,
};
use super::{AuthMode, AuthStrategy, Credential, LoginOptions, ProviderId};
use crate::error::{AgentError, AgentResult};

/// Client-side expiry skew (seconds): refresh a little before real expiry.
const EXPIRY_SKEW_SECS: u64 = 5 * 60;

/// Build an [`AuthStrategy`] for `provider`.
pub fn strategy_for(provider: ProviderId) -> Box<dyn AuthStrategy> {
    match provider {
        ProviderId::Anthropic => Box::new(AnthropicAuth),
        ProviderId::OpenAi => Box::new(OpenAiAuth),
        ProviderId::Xai => Box::new(XaiAuth),
    }
}

/// Build an API-key [`Credential`] (no OAuth). `base_url` falls back to the
/// provider default when empty.
pub fn from_api_key(provider: ProviderId, key: &str, base_url: Option<&str>) -> Credential {
    let base_url = base_url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| provider.default_base_url())
        .trim_end_matches('/')
        .to_string();
    Credential {
        provider,
        mode: AuthMode::ApiKey,
        access_token: key.trim().to_string(),
        refresh_token: None,
        expires_at_unix: None,
        account_id: None,
        base_url,
    }
}

/// Fixed parameters describing a loopback OAuth endpoint configuration.
struct Loopback {
    host: &'static str,
    port: u16,
    path: &'static str,
    /// Allow falling back to an ephemeral port when `port` is busy.
    allow_fallback: bool,
}

/// The fruit of a completed authorization-code exchange step: the raw code,
/// the redirect URI it was issued against, and the PKCE verifier to redeem it.
struct CodeGrant {
    code: String,
    redirect_uri: String,
    verifier: String,
}

/// Run the shared loopback/manual-paste dance and return the validated code,
/// the redirect URI actually advertised, and the PKCE verifier.
fn obtain_code(
    loopback: &Loopback,
    opts: &LoginOptions,
    build_authorize_url: impl FnOnce(&str, &str, &Pkce) -> AgentResult<String>,
) -> AgentResult<CodeGrant> {
    let pkce = Pkce::generate();
    let state = oauth::random_urlsafe(24);

    let server = if opts.manual_paste {
        None
    } else {
        Some(oauth::start_loopback_server(
            loopback.host,
            loopback.port,
            loopback.path,
            loopback.allow_fallback,
        )?)
    };
    let redirect_uri = match &server {
        Some(server) => server.redirect_uri.clone(),
        None => format!(
            "http://{}:{}{}",
            loopback.host, loopback.port, loopback.path
        ),
    };

    let authorize_url = build_authorize_url(&redirect_uri, &state, &pkce)?;
    announce_and_open(&authorize_url, opts.no_browser || opts.manual_paste);

    let callback = match &server {
        Some(server) => {
            println!();
            println!("Waiting for callback on {redirect_uri}");
            oauth::wait_for_callback(
                server,
                loopback.path,
                opts.timeout,
                &state,
                &nerve_core::CancelToken::new(),
            )?
        }
        None => oauth::prompt_manual_callback()?,
    };
    oauth::validate_callback(&callback, &state)?;
    let code = oauth::require_code(&callback)?;
    Ok(CodeGrant {
        code,
        redirect_uri,
        verifier: pkce.verifier,
    })
}

// ----------------------------------------------------------------------------
// Anthropic (Claude Pro/Max)
// ----------------------------------------------------------------------------

const ANTHROPIC_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const ANTHROPIC_TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
// base64("9d1c250a-e61b-44d9-88ed-5944d1962f5e"); kept decoded to avoid a build dep.
const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const ANTHROPIC_SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
const ANTHROPIC_BETA: &str = "oauth-2025-04-20";
const ANTHROPIC_REDIRECT_HOST: &str = "localhost";
const ANTHROPIC_REDIRECT_PORT: u16 = 54_545;
const ANTHROPIC_REDIRECT_PATH: &str = "/callback";

struct AnthropicAuth;

impl AnthropicAuth {
    fn loopback() -> Loopback {
        Loopback {
            host: ANTHROPIC_REDIRECT_HOST,
            port: ANTHROPIC_REDIRECT_PORT,
            path: ANTHROPIC_REDIRECT_PATH,
            allow_fallback: true,
        }
    }

    fn authorize_url(redirect_uri: &str, state: &str, pkce: &Pkce) -> AgentResult<String> {
        let mut url = url::Url::parse(ANTHROPIC_AUTHORIZE_URL)
            .map_err(|err| AgentError::Auth(err.to_string()))?;
        url.query_pairs_mut()
            .append_pair("code", "true")
            .append_pair("client_id", ANTHROPIC_CLIENT_ID)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", ANTHROPIC_SCOPES)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state);
        Ok(url.to_string())
    }

    fn credential_from_token(value: &Value) -> AgentResult<Credential> {
        let access_token = required_str(value, "access_token", "Anthropic token")?;
        let refresh_token = oauth::optional_str(value, "refresh_token");
        let expires_in = value
            .get("expires_in")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                AgentError::Parse("Anthropic token response missing `expires_in`".into())
            })?;
        let account = value.get("account");
        let account_id = account
            .and_then(|account| account.get("uuid"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned);
        Ok(Credential {
            provider: ProviderId::Anthropic,
            mode: AuthMode::Oauth,
            access_token,
            refresh_token,
            expires_at_unix: Some(oauth::expires_at(expires_in, EXPIRY_SKEW_SECS)),
            account_id,
            base_url: ProviderId::Anthropic.default_base_url().to_string(),
        })
    }
}

impl AuthStrategy for AnthropicAuth {
    fn provider(&self) -> ProviderId {
        ProviderId::Anthropic
    }

    fn login(&self, opts: &LoginOptions) -> AgentResult<Credential> {
        let grant = obtain_code(&Self::loopback(), opts, Self::authorize_url)?;
        // Anthropic's redirect can append `#state` to the code; split it off and
        // use the fragment as the exchange `state` when present.
        let (code, state) = match grant.code.split_once('#') {
            Some((code, state)) if !state.is_empty() => (code.to_string(), state.to_string()),
            _ => (grant.code.clone(), String::new()),
        };
        let body = json!({
            "grant_type": "authorization_code",
            "client_id": ANTHROPIC_CLIENT_ID,
            "code": code,
            "state": state,
            "redirect_uri": grant.redirect_uri,
            "code_verifier": grant.verifier,
        });
        let value = post_token_json(ANTHROPIC_TOKEN_URL, &[], &body)?;
        Self::credential_from_token(&value)
    }

    fn refresh(&self, cred: &Credential) -> AgentResult<Credential> {
        let refresh_token = cred
            .refresh_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| AgentError::Auth("Anthropic credential has no refresh_token".into()))?;
        let body = json!({
            "grant_type": "refresh_token",
            "client_id": ANTHROPIC_CLIENT_ID,
            "refresh_token": refresh_token,
        });
        let headers = [
            ("anthropic-beta".to_string(), ANTHROPIC_BETA.to_string()),
            (
                "User-Agent".to_string(),
                "anthropic-sdk-typescript/0.94.0 userOAuthProvider".to_string(),
            ),
        ];
        let value = post_token_json(ANTHROPIC_TOKEN_URL, &headers, &body)?;
        let mut refreshed = Self::credential_from_token(&value)?;
        if refreshed.refresh_token.is_none() {
            refreshed.refresh_token = Some(refresh_token.to_string());
        }
        Ok(refreshed)
    }
}

// ----------------------------------------------------------------------------
// OpenAI (ChatGPT / Codex)
// ----------------------------------------------------------------------------

const OPENAI_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
const OPENAI_ORIGINATOR: &str = "nerve";
const OPENAI_REDIRECT_HOST: &str = "localhost";
const OPENAI_REDIRECT_PORT: u16 = 1455;
const OPENAI_REDIRECT_PATH: &str = "/auth/callback";
const OPENAI_JWT_AUTH_CLAIM: &str = "https://api.openai.com/auth";
const OPENAI_JWT_PROFILE_CLAIM: &str = "https://api.openai.com/profile";

struct OpenAiAuth;

impl OpenAiAuth {
    fn loopback() -> Loopback {
        Loopback {
            host: OPENAI_REDIRECT_HOST,
            port: OPENAI_REDIRECT_PORT,
            path: OPENAI_REDIRECT_PATH,
            // OpenAI only allows the fixed redirect; never fall back.
            allow_fallback: false,
        }
    }

    fn authorize_url(redirect_uri: &str, state: &str, pkce: &Pkce) -> AgentResult<String> {
        let mut url = url::Url::parse(OPENAI_AUTHORIZE_URL)
            .map_err(|err| AgentError::Auth(err.to_string()))?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", OPENAI_CLIENT_ID)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", OPENAI_SCOPE)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state)
            .append_pair("id_token_add_organizations", "true")
            .append_pair("codex_cli_simplified_flow", "true")
            .append_pair("originator", OPENAI_ORIGINATOR);
        Ok(url.to_string())
    }

    /// Extract `(account_id, email)` from the access token's JWT claims.
    fn token_profile(access_token: &str) -> (Option<String>, Option<String>) {
        let Some(claims) = oauth::decode_jwt_claims(access_token) else {
            return (None, None);
        };
        let account_id = claims
            .get(OPENAI_JWT_AUTH_CLAIM)
            .and_then(|auth| auth.get("chatgpt_account_id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned);
        let email = claims
            .get(OPENAI_JWT_PROFILE_CLAIM)
            .and_then(|profile| profile.get("email"))
            .and_then(Value::as_str)
            .map(|email| email.trim().to_lowercase())
            .filter(|email| !email.is_empty());
        (account_id, email)
    }

    fn credential_from_token(value: &Value, require_account: bool) -> AgentResult<Credential> {
        let access_token = required_str(value, "access_token", "OpenAI token")?;
        let refresh_token = optional_str(value, "refresh_token");
        let expires_in = value
            .get("expires_in")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                AgentError::Parse("OpenAI token response missing `expires_in`".into())
            })?;
        let (account_id, _email) = Self::token_profile(&access_token);
        if require_account && account_id.is_none() {
            return Err(AgentError::Auth(
                "OpenAI token did not contain a chatgpt_account_id".into(),
            ));
        }
        Ok(Credential {
            provider: ProviderId::OpenAi,
            mode: AuthMode::Oauth,
            access_token,
            refresh_token,
            expires_at_unix: Some(oauth::expires_at(expires_in, 0)),
            account_id,
            base_url: ProviderId::OpenAi.default_base_url().to_string(),
        })
    }
}

impl AuthStrategy for OpenAiAuth {
    fn provider(&self) -> ProviderId {
        ProviderId::OpenAi
    }

    fn login(&self, opts: &LoginOptions) -> AgentResult<Credential> {
        let grant = obtain_code(&Self::loopback(), opts, Self::authorize_url)?;
        let form = [
            ("grant_type", "authorization_code"),
            ("client_id", OPENAI_CLIENT_ID),
            ("code", grant.code.as_str()),
            ("code_verifier", grant.verifier.as_str()),
            ("redirect_uri", grant.redirect_uri.as_str()),
        ];
        let value = post_token_form(OPENAI_TOKEN_URL, &form)?;
        Self::credential_from_token(&value, true)
    }

    fn refresh(&self, cred: &Credential) -> AgentResult<Credential> {
        let refresh_token = cred
            .refresh_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| AgentError::Auth("OpenAI credential has no refresh_token".into()))?;
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", OPENAI_CLIENT_ID),
        ];
        let value = post_token_form(OPENAI_TOKEN_URL, &form)?;
        let mut refreshed = Self::credential_from_token(&value, false)?;
        if refreshed.refresh_token.is_none() {
            refreshed.refresh_token = Some(refresh_token.to_string());
        }
        if refreshed.account_id.is_none() {
            refreshed.account_id = cred.account_id.clone();
        }
        Ok(refreshed)
    }
}

// ----------------------------------------------------------------------------
// xAI (Grok / SuperGrok)
// ----------------------------------------------------------------------------

const XAI_DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const XAI_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const XAI_REDIRECT_HOST: &str = "127.0.0.1";
const XAI_REDIRECT_PORT: u16 = 56_121;
const XAI_REDIRECT_PATH: &str = "/callback";
const XAI_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);

struct XaiAuth;

/// The two endpoints xAI advertises via OIDC discovery.
struct XaiDiscovery {
    authorization_endpoint: String,
    token_endpoint: String,
}

impl XaiAuth {
    fn loopback() -> Loopback {
        Loopback {
            host: XAI_REDIRECT_HOST,
            port: XAI_REDIRECT_PORT,
            path: XAI_REDIRECT_PATH,
            // xAI's redirect_uri allowlist requires the fixed loopback port.
            allow_fallback: false,
        }
    }

    /// Fetch and validate xAI's OIDC discovery document.
    fn discover() -> AgentResult<XaiDiscovery> {
        let agent = crate::provider::http::http_agent(XAI_DISCOVERY_TIMEOUT);
        let mut response = agent
            .get(XAI_DISCOVERY_URL)
            .header("Accept", "application/json")
            .call()
            .map_err(|err| AgentError::Http(format!("xAI OIDC discovery failed: {err}")))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|err| AgentError::Http(err.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(AgentError::Http(format!(
                "xAI OIDC discovery returned HTTP {status}: {text}"
            )));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|err| AgentError::Parse(format!("xAI discovery invalid JSON: {err}")))?;
        let authorization_endpoint =
            required_str(&value, "authorization_endpoint", "xAI discovery")?;
        let token_endpoint = required_str(&value, "token_endpoint", "xAI discovery")?;
        validate_xai_endpoint(&authorization_endpoint, "authorization_endpoint")?;
        validate_xai_endpoint(&token_endpoint, "token_endpoint")?;
        Ok(XaiDiscovery {
            authorization_endpoint,
            token_endpoint,
        })
    }

    fn authorize_url(
        authorization_endpoint: &str,
        redirect_uri: &str,
        state: &str,
        pkce: &Pkce,
    ) -> AgentResult<String> {
        let nonce = oauth::random_urlsafe(24);
        let mut url = url::Url::parse(authorization_endpoint)
            .map_err(|err| AgentError::Auth(err.to_string()))?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", XAI_CLIENT_ID)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", XAI_SCOPE)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state)
            .append_pair("nonce", &nonce)
            .append_pair("plan", "generic")
            .append_pair("referrer", "nerve");
        Ok(url.to_string())
    }

    fn credential_from_token(value: &Value) -> AgentResult<Credential> {
        let access_token = required_str(value, "access_token", "xAI token")?;
        let refresh_token = optional_str(value, "refresh_token");
        let expires_in = value
            .get("expires_in")
            .and_then(Value::as_u64)
            .ok_or_else(|| AgentError::Parse("xAI token response missing `expires_in`".into()))?;
        Ok(Credential {
            provider: ProviderId::Xai,
            mode: AuthMode::Oauth,
            access_token,
            refresh_token,
            expires_at_unix: Some(oauth::expires_at(expires_in, EXPIRY_SKEW_SECS)),
            account_id: None,
            base_url: ProviderId::Xai.default_base_url().to_string(),
        })
    }
}

impl AuthStrategy for XaiAuth {
    fn provider(&self) -> ProviderId {
        ProviderId::Xai
    }

    fn login(&self, opts: &LoginOptions) -> AgentResult<Credential> {
        let discovery = Self::discover()?;
        let authorize = discovery.authorization_endpoint.clone();
        let grant = obtain_code(&Self::loopback(), opts, |redirect, state, pkce| {
            Self::authorize_url(&authorize, redirect, state, pkce)
        })?;
        validate_xai_endpoint(&discovery.token_endpoint, "token_endpoint")?;
        let form = [
            ("grant_type", "authorization_code"),
            ("client_id", XAI_CLIENT_ID),
            ("code", grant.code.as_str()),
            ("redirect_uri", grant.redirect_uri.as_str()),
            ("code_verifier", grant.verifier.as_str()),
        ];
        let value = post_token_form(&discovery.token_endpoint, &form)?;
        let cred = Self::credential_from_token(&value)?;
        if cred.refresh_token.is_none() {
            return Err(AgentError::Auth(
                "xAI token exchange response missing refresh_token".into(),
            ));
        }
        Ok(cred)
    }

    fn refresh(&self, cred: &Credential) -> AgentResult<Credential> {
        let refresh_token = cred
            .refresh_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| AgentError::Auth("xAI credential has no refresh_token".into()))?;
        let discovery = Self::discover()?;
        validate_xai_endpoint(&discovery.token_endpoint, "token_endpoint")?;
        let form = [
            ("grant_type", "refresh_token"),
            ("client_id", XAI_CLIENT_ID),
            ("refresh_token", refresh_token),
        ];
        let value = post_token_form(&discovery.token_endpoint, &form)?;
        let mut refreshed = Self::credential_from_token(&value)?;
        if refreshed.refresh_token.is_none() {
            refreshed.refresh_token = Some(refresh_token.to_string());
        }
        Ok(refreshed)
    }
}

/// Pin an xAI OIDC endpoint to HTTPS on the `x.ai` origin (or a `*.x.ai`
/// subdomain), with no query string. Guards against a poisoned discovery doc
/// silently redirecting refresh tokens to a foreign host.
fn validate_xai_endpoint(endpoint: &str, field: &str) -> AgentResult<()> {
    let invalid = || AgentError::Auth(format!("invalid xAI {field}: {endpoint}"));
    let parsed = url::Url::parse(endpoint).map_err(|_| invalid())?;
    if parsed.scheme() != "https" {
        return Err(invalid());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(invalid());
    }
    if parsed.query().is_some() {
        return Err(invalid());
    }
    let host = parsed
        .host_str()
        .map(str::to_ascii_lowercase)
        .ok_or_else(invalid)?;
    if host == "x.ai" || host.ends_with(".x.ai") {
        Ok(())
    } else {
        Err(invalid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_api_key_uses_default_base_url_when_absent() {
        let cred = from_api_key(ProviderId::Xai, "  sk-xai  ", None);
        assert_eq!(cred.mode, AuthMode::ApiKey);
        assert_eq!(cred.access_token, "sk-xai");
        assert_eq!(cred.base_url, "https://api.x.ai");
        assert!(cred.refresh_token.is_none());
        assert!(cred.expires_at_unix.is_none());
    }

    #[test]
    fn from_api_key_trims_trailing_slash_on_custom_base_url() {
        let cred = from_api_key(ProviderId::OpenAi, "k", Some("https://proxy.example/v1/"));
        assert_eq!(cred.base_url, "https://proxy.example/v1");
    }

    #[test]
    fn strategy_for_returns_matching_provider() {
        assert_eq!(
            strategy_for(ProviderId::Anthropic).provider(),
            ProviderId::Anthropic
        );
        assert_eq!(
            strategy_for(ProviderId::OpenAi).provider(),
            ProviderId::OpenAi
        );
        assert_eq!(strategy_for(ProviderId::Xai).provider(), ProviderId::Xai);
    }

    #[test]
    fn anthropic_authorize_url_has_pkce_and_scopes() {
        let pkce = Pkce::generate();
        let url = AnthropicAuth::authorize_url("http://localhost:54545/callback", "st4te", &pkce)
            .expect("url");
        assert!(url.starts_with("https://claude.ai/oauth/authorize?"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=st4te"));
        assert!(url.contains("code=true"));
        assert!(url.contains(&urlencoding_fragment(&pkce.challenge)));
    }

    #[test]
    fn openai_authorize_url_has_codex_flags() {
        let pkce = Pkce::generate();
        let url = OpenAiAuth::authorize_url("http://localhost:1455/auth/callback", "s", &pkce)
            .expect("url");
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("id_token_add_organizations=true"));
        assert!(url.contains("originator=nerve"));
    }

    #[test]
    fn openai_token_profile_reads_account_and_email() {
        use base64::Engine as _;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let claims = serde_json::json!({
            (OPENAI_JWT_AUTH_CLAIM): { "chatgpt_account_id": "acc_42" },
            (OPENAI_JWT_PROFILE_CLAIM): { "email": "User@Example.COM" },
        });
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let token = format!("{header}.{payload}.sig");
        let (account, email) = OpenAiAuth::token_profile(&token);
        assert_eq!(account.as_deref(), Some("acc_42"));
        assert_eq!(email.as_deref(), Some("user@example.com"));
    }

    #[test]
    fn anthropic_credential_extracts_account_uuid() {
        let value = serde_json::json!({
            "access_token": "at",
            "refresh_token": "rt",
            "expires_in": 3600,
            "account": { "uuid": "u-1", "email_address": "a@b.c" },
        });
        let cred = AnthropicAuth::credential_from_token(&value).expect("cred");
        assert_eq!(cred.account_id.as_deref(), Some("u-1"));
        assert_eq!(cred.refresh_token.as_deref(), Some("rt"));
        assert!(cred.expires_at_unix.is_some());
    }

    #[test]
    fn validate_xai_endpoint_pins_origin() {
        assert!(validate_xai_endpoint("https://auth.x.ai/token", "token_endpoint").is_ok());
        assert!(
            validate_xai_endpoint("https://accounts.x.ai/oauth", "authorization_endpoint").is_ok()
        );
        assert!(validate_xai_endpoint("https://example.com/token", "token_endpoint").is_err());
        assert!(validate_xai_endpoint("http://auth.x.ai/token", "token_endpoint").is_err());
        assert!(validate_xai_endpoint("https://auth.x.ai/token?x=1", "token_endpoint").is_err());
        assert!(
            validate_xai_endpoint("https://user:pw@auth.x.ai/token", "token_endpoint").is_err()
        );
    }

    /// Percent-encode just the characters `url::Url` escapes in a query value,
    /// so the assertion above can locate the challenge regardless of encoding.
    fn urlencoding_fragment(raw: &str) -> String {
        let mut url = url::Url::parse("https://x/").unwrap();
        url.query_pairs_mut().append_pair("c", raw);
        url.query().unwrap().trim_start_matches("c=").to_string()
    }
}
