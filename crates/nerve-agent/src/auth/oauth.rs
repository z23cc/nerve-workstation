//! Shared OAuth machinery: PKCE, randomness, the loopback redirect server,
//! manual-paste fallback, browser launching, and token-endpoint POST helpers.
//!
//! This mirrors the synchronous, dependency-light style used by the xAI OAuth
//! login in `crates/nerve-workstation/src/auth/` (tiny_http loopback server,
//! `rand::OsRng` for entropy, `sha2`/`base64` for PKCE). It is provider-neutral;
//! the per-provider endpoints and grant bodies live in [`super::strategy`].

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use rand::rngs::OsRng;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use url::form_urlencoded;

use crate::error::{AgentError, AgentResult};
use crate::provider::http::{http_agent, post_json};

/// Default overall timeout for a single token-endpoint exchange.
const TOKEN_TIMEOUT: Duration = Duration::from_secs(30);

/// A PKCE verifier/challenge pair (RFC 7636, S256).
pub struct Pkce {
    /// The high-entropy code verifier (sent on token exchange).
    pub verifier: String,
    /// The S256 challenge derived from the verifier (sent on authorize).
    pub challenge: String,
}

impl Pkce {
    /// Generate a fresh verifier (96 random bytes, base64url-no-pad) and its
    /// SHA-256 challenge, matching the reference web-crypto flow.
    pub fn generate() -> Self {
        let verifier = random_urlsafe(96);
        let challenge = s256_challenge(&verifier);
        Self {
            verifier,
            challenge,
        }
    }
}

/// Compute the base64url-no-pad SHA-256 challenge for a PKCE `verifier`.
pub fn s256_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Return `len` cryptographically random bytes encoded as base64url-no-pad.
///
/// Used for the PKCE verifier and for CSRF `state` / OIDC `nonce` values.
pub fn random_urlsafe(len: usize) -> String {
    let mut bytes = vec![0_u8; len];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Current wall-clock time as whole seconds since the Unix epoch.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Convert an OAuth `expires_in` (seconds from now) into an absolute Unix
/// expiry, subtracting `skew_secs` so callers refresh slightly early.
pub fn expires_at(expires_in: u64, skew_secs: u64) -> u64 {
    now_unix()
        .saturating_add(expires_in)
        .saturating_sub(skew_secs)
}

/// Decode the JSON claims of a JWT's payload segment without verifying it.
///
/// Returns `None` for anything that is not a three-segment JWT with a
/// base64url-decodable JSON payload. Callers use this only to read identity
/// hints (e.g. an account id) from an access token — never for trust.
pub fn decode_jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.as_bytes()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// POST a JSON body to a token endpoint and decode the JSON response.
///
/// Used by providers (e.g. Anthropic) whose OAuth token endpoint speaks JSON.
pub fn post_token_json(
    url: &str,
    headers: &[(String, String)],
    body: &Value,
) -> AgentResult<Value> {
    let agent = http_agent(TOKEN_TIMEOUT);
    post_json(&agent, url, headers, body)
}

/// POST an `application/x-www-form-urlencoded` body to a token endpoint and
/// decode the JSON response.
///
/// Used by providers (OpenAI, xAI) whose OAuth token endpoint requires form
/// encoding. Mirrors `http_post_form_json` in the workstation auth module.
pub fn post_token_form(url: &str, form: &[(&str, &str)]) -> AgentResult<Value> {
    let agent = http_agent(TOKEN_TIMEOUT);
    let mut response = agent
        .post(url)
        .header("Accept", "application/json")
        .send_form(form.iter().copied())
        .map_err(|err| AgentError::Http(err.to_string()))?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|err| AgentError::Http(err.to_string()))?;
    if !(200..300).contains(&status) {
        return Err(AgentError::Http(format!("HTTP {status}: {text}")));
    }
    serde_json::from_str(&text)
        .map_err(|err| AgentError::Parse(format!("invalid JSON response: {err}: {text}")))
}

/// The redirect parameters captured from an OAuth callback.
pub struct OAuthCallback {
    /// The `code` query parameter, if present.
    pub code: Option<String>,
    /// The `state` query parameter, if present.
    pub state: Option<String>,
    /// The `error` query parameter, if the provider reported one.
    pub error: Option<String>,
    /// The `error_description` query parameter, if present.
    pub error_description: Option<String>,
    /// Whether this callback was pasted manually (state may be absent).
    pub manual_paste: bool,
}

/// A bound loopback HTTP server awaiting the OAuth redirect.
pub struct LoopbackServer {
    server: Server,
    /// The exact redirect URI the provider must redirect to.
    pub redirect_uri: String,
}

/// Bind a loopback redirect server on `host:port` (falling back to an ephemeral
/// port only when `allow_fallback` is set), serving `path`.
///
/// Providers with a fixed allowlisted redirect (OpenAI, xAI) must pass
/// `allow_fallback = false` so a busy port fails fast instead of producing a
/// redirect URI the provider will reject.
pub fn start_loopback_server(
    host: &str,
    port: u16,
    path: &str,
    allow_fallback: bool,
) -> AgentResult<LoopbackServer> {
    let listener = match TcpListener::bind((host, port)) {
        Ok(listener) => listener,
        Err(_) if allow_fallback => TcpListener::bind((host, 0))
            .map_err(|err| AgentError::Auth(format!("failed to bind OAuth loopback: {err}")))?,
        Err(err) => {
            return Err(AgentError::Auth(format!(
                "OAuth callback port {port} unavailable and fallback disabled: {err}"
            )));
        }
    };
    let bound_port = listener
        .local_addr()
        .map_err(|err| AgentError::Auth(err.to_string()))?
        .port();
    let server = Server::from_listener(listener, None)
        .map_err(|err| AgentError::Auth(format!("failed to start OAuth loopback server: {err}")))?;
    Ok(LoopbackServer {
        server,
        redirect_uri: format!("http://{host}:{bound_port}{path}"),
    })
}

/// Block until the loopback server receives a matching callback, the deadline
/// passes, or `cancel` fires.
pub fn wait_for_callback(
    server: &LoopbackServer,
    path: &str,
    timeout: Duration,
    expected_state: &str,
    cancel: &nerve_core::CancelToken,
) -> AgentResult<OAuthCallback> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(AgentError::Auth(
                "timed out waiting for OAuth callback".into(),
            ));
        }
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(250));
        match server.server.recv_timeout(wait) {
            Ok(Some(request)) => {
                if let Some(cb) = handle_callback_request(request, path, expected_state)? {
                    return Ok(cb);
                }
            }
            Ok(None) => {}
            Err(err) => {
                return Err(AgentError::Auth(format!(
                    "failed while waiting for OAuth callback: {err}"
                )));
            }
        }
    }
}

fn handle_callback_request(
    request: Request,
    path: &str,
    expected_state: &str,
) -> AgentResult<Option<OAuthCallback>> {
    let method = request.method().clone();
    let target = request.url().to_string();
    if method == Method::Options {
        respond_http(request, 204, "")?;
        return Ok(None);
    }
    if method != Method::Get {
        respond_http(request, 405, "method not allowed")?;
        return Ok(None);
    }
    if target.split('?').next() != Some(path) {
        respond_http(request, 404, "not found")?;
        return Ok(None);
    }
    let Some(cb) = parse_callback_target(&target, false) else {
        respond_http(request, 400, "OAuth callback missing query parameters")?;
        return Ok(None);
    };
    if cb.error.is_none() && cb.code.is_none() {
        respond_http(request, 400, "OAuth callback missing code or error")?;
        return Ok(None);
    }
    if cb.state.as_deref() != Some(expected_state) {
        respond_http(request, 400, "OAuth callback state mismatch")?;
        return Ok(None);
    }
    respond_http(request, 200, "Nerve login complete; return to the terminal")?;
    Ok(Some(cb))
}

fn respond_http(request: Request, status: u16, message: &str) -> AgentResult<()> {
    let body = if status == 204 {
        String::new()
    } else {
        format!("<html><body><p>{message}</p></body></html>")
    };
    let mut response = Response::from_string(body).with_status_code(StatusCode(status));
    for (name, value) in [
        ("Content-Type", "text/html; charset=utf-8"),
        ("Access-Control-Allow-Origin", "*"),
        ("Access-Control-Allow-Methods", "GET, OPTIONS"),
        ("Access-Control-Allow-Headers", "*"),
        ("Connection", "close"),
    ] {
        response.add_header(static_header(name, value));
    }
    request
        .respond(response)
        .map_err(|err| AgentError::Auth(format!("failed to write OAuth callback response: {err}")))
}

fn static_header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("static HTTP header is valid")
}

/// Prompt on stdin for a pasted callback URL or raw authorization code.
pub fn prompt_manual_callback() -> AgentResult<OAuthCallback> {
    println!();
    println!("Paste the full callback URL or authorization code, then press Enter:");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|err| AgentError::Auth(format!("failed to read callback input: {err}")))?;
    Ok(parse_pasted_callback(input.trim()))
}

/// Parse a manually pasted callback: either a full/partial redirect URL with a
/// query string, or a bare authorization code (optionally `code#state`).
pub fn parse_pasted_callback(input: &str) -> OAuthCallback {
    if input.contains("code=") || input.contains("error=") {
        let path = input.find('?').map_or(input, |idx| &input[idx..]);
        if let Some(cb) = parse_callback_target(path, true) {
            return cb;
        }
    }
    let (code, state) = match input.split_once('#') {
        Some((code, state)) if !state.is_empty() => (code.to_string(), Some(state.to_string())),
        _ => (input.to_string(), None),
    };
    OAuthCallback {
        code: Some(code),
        state,
        error: None,
        error_description: None,
        manual_paste: true,
    }
}

fn parse_callback_target(target: &str, manual_paste: bool) -> Option<OAuthCallback> {
    let (_, query) = target.split_once('?')?;
    let params: BTreeMap<String, String> = form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    Some(OAuthCallback {
        code: params.get("code").cloned(),
        state: params.get("state").cloned(),
        error: params.get("error").cloned(),
        error_description: params.get("error_description").cloned(),
        manual_paste,
    })
}

/// Validate a callback's `state` against the expected CSRF token and surface
/// any provider-reported error.
pub fn validate_callback(cb: &OAuthCallback, expected_state: &str) -> AgentResult<()> {
    if let Some(error) = &cb.error {
        let description = cb.error_description.as_deref().unwrap_or(error);
        return Err(AgentError::Auth(format!(
            "authorization failed: {description}"
        )));
    }
    if cb.state.as_deref() == Some(expected_state) {
        return Ok(());
    }
    // A manually pasted bare code legitimately carries no state.
    if cb.manual_paste && cb.state.is_none() {
        return Ok(());
    }
    Err(AgentError::Auth(
        "authorization failed: state mismatch".into(),
    ))
}

/// Extract the trimmed, non-empty authorization code from a validated callback.
pub fn require_code(cb: &OAuthCallback) -> AgentResult<String> {
    cb.code
        .as_deref()
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| AgentError::Auth("authorization failed: missing authorization code".into()))
}

/// Print the authorize URL and best-effort open it in the system browser.
pub fn announce_and_open(authorize_url: &str, no_browser: bool) {
    println!("Open this URL to authorize Nerve:");
    println!("{authorize_url}");
    if !no_browser {
        try_open_browser(authorize_url);
    }
}

/// Best-effort launch of the platform browser opener. Never fails the flow:
/// the URL has already been printed for manual use.
fn try_open_browser(url: &str) {
    let spawned = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).spawn().is_ok()
    } else if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .is_ok()
    } else {
        Command::new("xdg-open").arg(url).spawn().is_ok()
    };
    if !spawned {
        println!("Could not open the browser automatically; use the URL above.");
    }
}

/// Pull a required, trimmed, non-empty string field out of a JSON token
/// response, attributing failures to `label`.
pub fn required_str(value: &Value, key: &str, label: &str) -> AgentResult<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| AgentError::Parse(format!("{label} response missing `{key}`")))
}

/// Pull an optional, trimmed, non-empty string field out of a JSON value.
pub fn optional_str(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s256_challenge_matches_rfc7636_example() {
        // RFC 7636 Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            s256_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn pkce_generate_roundtrips() {
        let pkce = Pkce::generate();
        assert!(!pkce.verifier.is_empty());
        assert_eq!(s256_challenge(&pkce.verifier), pkce.challenge);
    }

    #[test]
    fn random_urlsafe_is_url_safe_and_distinct() {
        let a = random_urlsafe(24);
        let b = random_urlsafe(24);
        assert_ne!(a, b);
        assert!(URL_SAFE_NO_PAD.decode(a.as_bytes()).is_ok());
    }

    #[test]
    fn decode_jwt_claims_reads_payload() {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"acct_123"}"#);
        let token = format!("{header}.{payload}.sig");
        let claims = decode_jwt_claims(&token).expect("claims");
        assert_eq!(claims.get("sub").and_then(Value::as_str), Some("acct_123"));
        assert!(decode_jwt_claims("not-a-jwt").is_none());
    }

    #[test]
    fn parse_pasted_full_url_extracts_code_and_state() {
        let cb = parse_pasted_callback("http://localhost:1455/auth/callback?code=abc&state=xyz");
        assert_eq!(cb.code.as_deref(), Some("abc"));
        assert_eq!(cb.state.as_deref(), Some("xyz"));
    }

    #[test]
    fn parse_pasted_bare_code_splits_fragment_state() {
        let cb = parse_pasted_callback("rawcode#somestate");
        assert_eq!(cb.code.as_deref(), Some("rawcode"));
        assert_eq!(cb.state.as_deref(), Some("somestate"));
        assert!(cb.manual_paste);
    }

    #[test]
    fn validate_callback_accepts_matching_state_and_rejects_mismatch() {
        let ok = OAuthCallback {
            code: Some("c".into()),
            state: Some("s".into()),
            error: None,
            error_description: None,
            manual_paste: false,
        };
        assert!(validate_callback(&ok, "s").is_ok());
        assert!(validate_callback(&ok, "other").is_err());
    }

    #[test]
    fn validate_callback_surfaces_provider_error() {
        let err = OAuthCallback {
            code: None,
            state: None,
            error: Some("access_denied".into()),
            error_description: Some("user said no".into()),
            manual_paste: false,
        };
        let result = validate_callback(&err, "s");
        assert!(matches!(result, Err(AgentError::Auth(msg)) if msg.contains("user said no")));
    }

    #[test]
    fn expires_at_subtracts_skew() {
        let now = now_unix();
        let exp = expires_at(3600, 300);
        assert!(exp >= now + 3600 - 300 - 2 && exp <= now + 3600 - 300 + 2);
    }
}
