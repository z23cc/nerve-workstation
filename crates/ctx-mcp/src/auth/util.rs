use super::*;
#[cfg(test)]
use oauth2::{PkceCodeChallenge, PkceCodeVerifier};
use url::Url;

pub(super) fn required_string(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("response was missing required field `{key}`"))
}

pub(super) fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

pub(super) fn validate_oauth_endpoint(url: &str, field: &str) -> Result<()> {
    let uri = parse_https_xai_uri(url, field)?;
    if has_query(&uri) {
        bail!("{field} must not include a query string: {url}");
    }
    Ok(())
}

pub(super) fn validate_inference_base_url(value: Option<&str>) -> Result<String> {
    let candidate = value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(DEFAULT_BASE_URL)
        .trim_end_matches('/');
    let uri = parse_https_xai_uri(candidate, "xAI base_url")?;
    let host = uri
        .host_str()
        .ok_or_else(|| anyhow!("xAI base_url must include a host: {candidate}"))?;
    if !is_xai_api_host(host) {
        bail!("xAI base_url host `{host}` must be an xAI API subdomain");
    }
    if has_query(&uri) {
        bail!("xAI base_url must not include a query string: {candidate}");
    }
    Ok(candidate.to_string())
}

pub(super) fn validate_loopback_redirect_uri(uri: &str) -> Result<()> {
    if !uri.starts_with(&format!("http://{REDIRECT_HOST}:")) || !uri.ends_with(REDIRECT_PATH) {
        bail!("xAI redirect_uri must be a loopback URL on {REDIRECT_HOST}{REDIRECT_PATH}");
    }
    Ok(())
}

pub(super) fn parse_https_xai_uri(value: &str, label: &str) -> Result<Url> {
    let uri = Url::parse(value).with_context(|| format!("{label} must be a valid URL: {value}"))?;
    if uri.scheme() != "https" {
        bail!("{label} must be an HTTPS URL: {value}");
    }
    if !uri.username().is_empty() || uri.password().is_some() {
        bail!("{label} must not include userinfo: {value}");
    }
    let host = uri
        .host_str()
        .ok_or_else(|| anyhow!("{label} must include a host: {value}"))?;
    if !is_xai_host(host) {
        bail!("{label} host `{host}` is not on the xAI origin");
    }
    Ok(uri)
}

pub(super) fn has_query(uri: &Url) -> bool {
    uri.query().is_some()
}

pub(super) fn is_xai_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "x.ai" || host.ends_with(".x.ai")
}

pub(super) fn is_xai_api_host(host: &str) -> bool {
    host.to_ascii_lowercase().ends_with(".x.ai")
}

pub(super) fn env_base_url() -> Option<String> {
    std::env::var("CTX_MCP_XAI_BASE_URL")
        .ok()
        .or_else(|| std::env::var("XAI_BASE_URL").ok())
}

pub(super) fn client_id() -> String {
    std::env::var("CTX_MCP_XAI_OAUTH_CLIENT_ID").unwrap_or_else(|_| CLIENT_ID.to_string())
}

pub(super) fn preferred_redirect_uri() -> String {
    format!("http://{REDIRECT_HOST}:{REDIRECT_PORT}{REDIRECT_PATH}")
}

pub(super) fn random_urlsafe(bytes_len: usize) -> String {
    let mut bytes = vec![0_u8; bytes_len];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
pub(super) fn pkce_challenge(verifier: &str) -> String {
    let verifier = PkceCodeVerifier::new(verifier.to_string());
    PkceCodeChallenge::from_code_verifier_sha256(&verifier)
        .as_str()
        .to_string()
}

pub(super) fn access_token_is_expiring(token: &str, skew_seconds: u64) -> bool {
    jwt_expiry(token).is_some_and(|exp| exp <= now_unix().saturating_add(skew_seconds))
}

pub(super) fn jwt_expiry(token: &str) -> Option<u64> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.as_bytes()).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value.get("exp").and_then(Value::as_u64)
}

pub(super) fn expiry_label(expiry: u64) -> &'static str {
    if expiry <= now_unix() {
        "expired"
    } else if expiry <= now_unix().saturating_add(REFRESH_SKEW_SECONDS) {
        "expiring"
    } else {
        "valid"
    }
}

pub(super) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
