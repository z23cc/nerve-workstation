use super::callback::{
    prompt_manual_callback, start_loopback_server, try_open_browser, validate_callback,
    wait_for_callback,
};
use super::commands::LoginArgs;
use super::http::{http_get_json, http_post_form_json};
use super::util::{
    client_id, env_base_url, optional_string, preferred_redirect_uri, random_urlsafe,
    required_string, validate_inference_base_url, validate_loopback_redirect_uri,
    validate_oauth_endpoint,
};
use super::*;
use oauth2::{
    AuthUrl, ClientId, CsrfToken, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    TokenUrl, basic::BasicClient,
};

pub(super) fn run_loopback_login(args: &LoginArgs) -> Result<LoginCredentials> {
    let discovery = discover_xai(Duration::from_secs(args.timeout_seconds))?;
    let pkce = Pkce::generate();
    let state = random_urlsafe(24);
    let nonce = random_urlsafe(24);
    let (redirect_uri, server) = if args.manual_paste {
        (preferred_redirect_uri(), None)
    } else {
        let server = start_loopback_server()?;
        (server.redirect_uri.clone(), Some(server))
    };
    validate_loopback_redirect_uri(&redirect_uri)?;
    let authorize_url = build_authorize_url(&discovery, &redirect_uri, &pkce, &state, &nonce)?;

    println!("Open this URL to authorize Nerve with xAI:");
    println!("{authorize_url}");
    if !args.no_browser && !args.manual_paste {
        try_open_browser(&authorize_url);
    }

    let callback = if let Some(server) = server {
        println!();
        println!("Waiting for callback on {redirect_uri}");
        wait_for_callback(
            server,
            Duration::from_secs(args.timeout_seconds.max(30)),
            &state,
        )?
    } else {
        prompt_manual_callback()?
    };
    validate_callback(&callback, &state)?;
    let code = callback
        .code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("xAI authorization failed: missing authorization code"))?;
    let tokens = exchange_code_for_tokens(
        &discovery,
        code,
        &redirect_uri,
        pkce.verifier.secret(),
        pkce.challenge.as_str(),
        Duration::from_secs(args.timeout_seconds),
    )?;
    Ok(LoginCredentials {
        tokens,
        discovery,
        redirect_uri,
        base_url: validate_inference_base_url(env_base_url().as_deref())?,
    })
}
pub(super) struct LoginCredentials {
    pub(super) tokens: XaiTokens,
    pub(super) discovery: XaiDiscovery,
    pub(super) redirect_uri: String,
    pub(super) base_url: String,
}

#[derive(Debug)]
struct Pkce {
    verifier: PkceCodeVerifier,
    challenge: PkceCodeChallenge,
}

impl Pkce {
    fn generate() -> Self {
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        Self {
            verifier,
            challenge,
        }
    }
}

pub(super) fn discover_xai(timeout: Duration) -> Result<XaiDiscovery> {
    let value = http_get_json(DISCOVERY_URL, timeout).context("xAI OIDC discovery failed")?;
    let authorization_endpoint = required_string(&value, "authorization_endpoint")?;
    let token_endpoint = required_string(&value, "token_endpoint")?;
    validate_oauth_endpoint(&authorization_endpoint, "authorization_endpoint")?;
    validate_oauth_endpoint(&token_endpoint, "token_endpoint")?;
    Ok(XaiDiscovery {
        authorization_endpoint,
        token_endpoint,
    })
}

fn build_authorize_url(
    discovery: &XaiDiscovery,
    redirect_uri: &str,
    pkce: &Pkce,
    state: &str,
    nonce: &str,
) -> Result<String> {
    let client = BasicClient::new(ClientId::new(client_id()))
        .set_auth_uri(AuthUrl::new(discovery.authorization_endpoint.clone())?)
        .set_token_uri(TokenUrl::new(discovery.token_endpoint.clone())?)
        .set_redirect_uri(RedirectUrl::new(redirect_uri.to_string())?);
    let request = client
        .authorize_url(|| CsrfToken::new(state.to_string()))
        .add_scopes(
            SCOPE
                .split_whitespace()
                .map(|scope| Scope::new(scope.to_string())),
        )
        .set_pkce_challenge(pkce.challenge.clone())
        .add_extra_param("nonce", nonce.to_string())
        .add_extra_param("plan", "generic")
        .add_extra_param("referrer", "nerve");
    let (url, _csrf) = request.url();
    Ok(url.to_string())
}

pub(super) fn exchange_code_for_tokens(
    discovery: &XaiDiscovery,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
    challenge: &str,
    timeout: Duration,
) -> Result<XaiTokens> {
    let client_id = client_id();
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id.as_str()),
        ("code_verifier", verifier),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
    ];
    let value = http_post_form_json(&discovery.token_endpoint, &form, timeout)
        .context("xAI token exchange failed")?;
    parse_token_response(value, "xAI token exchange")
}

pub(super) fn refresh_tokens(state: &XaiProviderState, tokens: &XaiTokens) -> Result<XaiTokens> {
    let endpoint = match &state.discovery {
        Some(discovery) => discovery.token_endpoint.clone(),
        None => discover_xai(Duration::from_secs(20))?.token_endpoint,
    };
    validate_oauth_endpoint(&endpoint, "token_endpoint")?;
    let client_id = client_id();
    let form = [
        ("grant_type", "refresh_token"),
        ("client_id", client_id.as_str()),
        ("refresh_token", tokens.refresh_token.as_str()),
    ];
    let value = http_post_form_json(&endpoint, &form, Duration::from_secs(20))
        .context("xAI token refresh failed")?;
    let mut refreshed = parse_token_response(value, "xAI token refresh")?;
    if refreshed.refresh_token.is_empty() {
        refreshed.refresh_token = tokens.refresh_token.clone();
    }
    Ok(refreshed)
}

pub(super) fn parse_token_response(value: Value, label: &str) -> Result<XaiTokens> {
    let access_token = required_string(&value, "access_token")?;
    let refresh_token = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if refresh_token.is_empty() && label.contains("exchange") {
        bail!("{label} response was missing refresh_token");
    }
    Ok(XaiTokens {
        access_token,
        refresh_token,
        id_token: optional_string(&value, "id_token"),
        expires_in: value.get("expires_in").and_then(Value::as_u64),
        token_type: optional_string(&value, "token_type").or_else(|| Some("Bearer".to_string())),
    })
}
