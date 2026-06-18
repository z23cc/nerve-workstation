//! Shared blocking HTTP/SSE helpers used by every provider.
//!
//! This mirrors the synchronous `ureq` v3 style used elsewhere in the
//! workspace: an [`http_agent`] with sane defaults, a [`post_json`] helper for
//! non-streaming exchanges (token refresh, etc.), and [`post_sse`] which opens
//! a streaming response and yields each `data:` payload via [`SseReader`].
//! Providers are responsible for parsing the JSON inside each event.

use std::io::{BufRead, BufReader, Read};
use std::time::Duration;

use serde_json::Value;

use crate::error::{AgentError, AgentResult};

/// User-Agent string sent with every request.
pub fn user_agent() -> String {
    format!("nerve-agent/{}", env!("CARGO_PKG_VERSION"))
}

/// Build a blocking HTTPS-only `ureq` agent with a global timeout.
pub fn http_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .http_status_as_error(false)
        .timeout_global(Some(timeout.max(Duration::from_secs(5))))
        .build()
        .into()
}

/// Apply a list of `(name, value)` headers to a request builder.
fn with_headers<Any>(
    mut req: ureq::RequestBuilder<Any>,
    headers: &[(String, String)],
) -> ureq::RequestBuilder<Any> {
    req = req.header("User-Agent", user_agent());
    for (name, value) in headers {
        req = req.header(name.as_str(), value.as_str());
    }
    req
}

/// POST a JSON body and decode the JSON response (non-streaming).
///
/// Used for OAuth token exchange/refresh and other one-shot calls.
pub fn post_json(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(String, String)],
    body: &Value,
) -> AgentResult<Value> {
    let req = with_headers(agent.post(url), headers).header("Accept", "application/json");
    let mut response = req
        .send_json(body)
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

/// POST a JSON body and open a streaming Server-Sent Events response.
///
/// On a non-2xx status the body is drained and returned as an [`AgentError::Http`].
pub fn post_sse(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(String, String)],
    body: &Value,
) -> AgentResult<SseReader> {
    let req = with_headers(agent.post(url), headers).header("Accept", "text/event-stream");
    let mut response = req
        .send_json(body)
        .map_err(|err| AgentError::Http(err.to_string()))?;

    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let text = response.body_mut().read_to_string().unwrap_or_default();
        return Err(AgentError::Http(format!("HTTP {status}: {text}")));
    }
    let reader = response.into_body().into_reader();
    Ok(SseReader {
        reader: BufReader::new(Box::new(reader)),
    })
}

/// A line-oriented reader over an SSE response body.
pub struct SseReader {
    reader: BufReader<Box<dyn Read>>,
}

impl SseReader {
    /// Return the next `data:` payload, or `None` at end of stream / `[DONE]`.
    ///
    /// Blank lines and comment lines (`:`) are skipped. The leading `data:`
    /// prefix and one optional space are stripped from the returned string.
    pub fn next_event(&mut self) -> AgentResult<Option<String>> {
        let mut line = String::new();
        loop {
            line.clear();
            let read = self
                .reader
                .read_line(&mut line)
                .map_err(|err| AgentError::Http(err.to_string()))?;
            if read == 0 {
                return Ok(None);
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() || trimmed.starts_with(':') {
                continue;
            }
            let Some(rest) = trimmed.strip_prefix("data:") else {
                // Non-data field (event:, id:, retry:) — skip it.
                continue;
            };
            let payload = rest.strip_prefix(' ').unwrap_or(rest);
            if payload == "[DONE]" {
                return Ok(None);
            }
            return Ok(Some(payload.to_string()));
        }
    }
}
