//! Minimal blocking JSON-over-HTTP helpers (ureq 3), mirroring nerve-agent's
//! `provider::http` idiom: an HTTPS-only agent with a global timeout, and
//! `POST`/`GET` helpers that return parsed JSON or a [`WeixinError`].

use crate::error::{WeixinError, WeixinResult};
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

/// Build a blocking HTTPS-only `ureq` agent with a global timeout (floored at 5s).
#[must_use]
pub fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .http_status_as_error(false)
        .timeout_global(Some(timeout.max(Duration::from_secs(5))))
        .build()
        .into()
}

/// POST a JSON body with the given headers and decode the JSON response.
pub(crate) fn post_json<B: Serialize>(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(String, String)],
    body: &B,
) -> WeixinResult<Value> {
    let mut req = agent
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json");
    for (name, value) in headers {
        req = req.header(name.as_str(), value.as_str());
    }
    let mut response = req
        .send_json(body)
        .map_err(|err| WeixinError::Transport(err.to_string()))?;
    read_json(response.status().as_u16(), &mut response)
}

/// GET a URL with the given headers and decode the JSON response.
pub(crate) fn get_json(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(String, String)],
) -> WeixinResult<Value> {
    let mut req = agent.get(url);
    for (name, value) in headers {
        req = req.header(name.as_str(), value.as_str());
    }
    let mut response = req
        .call()
        .map_err(|err| WeixinError::Transport(err.to_string()))?;
    read_json(response.status().as_u16(), &mut response)
}

/// Read a response body to a string and parse it as JSON, mapping a non-2xx status
/// to a transport error.
fn read_json(status: u16, response: &mut ureq::http::Response<ureq::Body>) -> WeixinResult<Value> {
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|err| WeixinError::Transport(err.to_string()))?;
    if !(200..300).contains(&status) {
        return Err(WeixinError::Transport(format!("HTTP {status}: {text}")));
    }
    serde_json::from_str(&text).map_err(|err| WeixinError::Parse(format!("{err}: {text}")))
}
