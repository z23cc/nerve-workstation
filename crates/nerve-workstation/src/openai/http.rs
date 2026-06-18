use crate::auth::{self, OpenAiCodexCredentials};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::time::Duration;

/// `originator` header value identifying the Codex executor client.
pub(super) const CODEX_ORIGINATOR: &str = "codex_exec";
/// `User-Agent` advertised in OAuth (Codex) mode, matching nerve-agent.
pub(super) const CODEX_USER_AGENT: &str =
    "codex_exec/0.139.0 (Windows 10.0.26200; x86_64) unknown (codex_exec; 0.139.0)";

pub(super) fn http_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .http_status_as_error(false)
        .timeout_global(Some(timeout.max(Duration::from_secs(5))))
        .build()
        .into()
}

pub(super) fn http_get_json(
    url: &str,
    creds: &OpenAiCodexCredentials,
    timeout: Duration,
) -> Result<Value> {
    let mut response = send_get_json(url, creds, timeout)?;
    if is_unauthorized(&response) {
        let refreshed = auth::resolve_openai_codex_credentials(true)?;
        response = send_get_json(url, &refreshed, timeout)?;
    }
    read_json_response(response)
}

pub(super) fn http_post_sse(
    url: &str,
    creds: &OpenAiCodexCredentials,
    payload: &Value,
    timeout: Duration,
) -> Result<String> {
    let mut response = send_post_sse(url, creds, payload, timeout)?;
    if is_unauthorized(&response) {
        let refreshed = auth::resolve_openai_codex_credentials(true)?;
        response = send_post_sse(url, &refreshed, payload, timeout)?;
    }
    ensure_success(&mut response)?;
    response
        .body_mut()
        .read_to_string()
        .map_err(|err| anyhow!(err.to_string()))
}

fn send_get_json(
    url: &str,
    creds: &OpenAiCodexCredentials,
    timeout: Duration,
) -> Result<ureq::http::Response<ureq::Body>> {
    let mut request = http_agent(timeout)
        .get(url)
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {}", creds.access_token))
        .header("originator", CODEX_ORIGINATOR)
        .header("User-Agent", CODEX_USER_AGENT);
    if let Some(account_id) = creds.account_id.as_deref() {
        request = request.header("chatgpt-account-id", account_id);
    }
    request.call().map_err(|err| anyhow!(err.to_string()))
}

fn send_post_sse(
    url: &str,
    creds: &OpenAiCodexCredentials,
    payload: &Value,
    timeout: Duration,
) -> Result<ureq::http::Response<ureq::Body>> {
    let mut request = http_agent(timeout)
        .post(url)
        .header("Accept", "text/event-stream")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", creds.access_token))
        .header("originator", CODEX_ORIGINATOR)
        .header("User-Agent", CODEX_USER_AGENT);
    if let Some(account_id) = creds.account_id.as_deref() {
        request = request.header("chatgpt-account-id", account_id);
    }
    request
        .send_json(payload)
        .map_err(|err| anyhow!(err.to_string()))
}

pub(super) fn read_json_response(mut response: ureq::http::Response<ureq::Body>) -> Result<Value> {
    ensure_success(&mut response)?;
    let body = response.body_mut().read_to_string().unwrap_or_default();
    serde_json::from_str(&body).with_context(|| format!("invalid JSON response: {body}"))
}

pub(super) fn ensure_success(response: &mut ureq::http::Response<ureq::Body>) -> Result<()> {
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    let body = response.body_mut().read_to_string().unwrap_or_default();
    if status == 401 {
        bail!(
            "OpenAI Codex returned HTTP 401; run `nerve agent login --provider chatgpt`. Response: {body}"
        );
    }
    if status == 403 {
        bail!(
            "OpenAI Codex returned HTTP 403; this ChatGPT account may lack entitlement. Response: {body}"
        );
    }
    bail!("OpenAI Codex returned HTTP {status}: {body}")
}

fn is_unauthorized(response: &ureq::http::Response<ureq::Body>) -> bool {
    response.status().as_u16() == 401
}
