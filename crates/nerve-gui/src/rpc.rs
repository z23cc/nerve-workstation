//! Protocol-v7 client plumbing for the WASM frontend: read the daemon bearer
//! token the served page injected, and POST JSON-RPC requests to `/rpc`.
//!
//! The token is injected by the daemon the same way the legacy `gui.html` is
//! served — as a `window.__NERVE_DAEMON_TOKEN__` global (see the daemon's
//! `render_app` token injection). On a remote bind the daemon does not embed
//! it, so the global is the unreplaced placeholder; in that case the operator
//! supplies it via the URL fragment and we fall back to that.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use gloo_net::http::Request;
use nerve_proto::RuntimeEvent;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{EventSource, MessageEvent};

/// The placeholder the daemon replaces with the real token on a loopback bind.
/// If we still see this string, no token was injected (remote bind).
const TOKEN_PLACEHOLDER: &str = "__NERVE_DAEMON_TOKEN__";

/// Read the daemon bearer token: prefer the injected `window.__NERVE_DAEMON_TOKEN__`
/// global; on a remote bind (placeholder unreplaced) fall back to a `#token=`
/// URL fragment. Returns `None` when neither yields a usable token.
pub fn daemon_token() -> Option<String> {
    if let Some(tok) = injected_token() {
        return Some(tok);
    }
    fragment_token()
}

/// The token baked into the page by the daemon, if it is a real (replaced) value.
fn injected_token() -> Option<String> {
    let window = web_sys::window()?;
    let value = js_sys::Reflect::get(&window, &"__NERVE_DAEMON_TOKEN__".into()).ok()?;
    let tok = value.as_string()?;
    (!tok.is_empty() && tok != TOKEN_PLACEHOLDER).then_some(tok)
}

/// A `#token=<tok>` URL fragment, used on a remote bind where the page carries
/// no embedded token (the operator opens the `#token=` URL the daemon printed).
fn fragment_token() -> Option<String> {
    let hash = web_sys::window()?.location().hash().ok()?;
    let hash = hash.strip_prefix('#').unwrap_or(&hash);
    hash.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == "token" && !v.is_empty()).then(|| v.to_string())
    })
}

/// One JSON-RPC call against `/rpc`, deserializing `result` into `T`.
///
/// `T` is a [`nerve_proto`] response type, so the WASM app shares the engine's
/// exact wire shape. Errors collapse to a human string for the placeholder UI;
/// richer error surfacing arrives with the real chat surface (G2).
pub async fn rpc_call<T: DeserializeOwned>(
    token: &str,
    method: &str,
    params: Value,
) -> Result<T, String> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let response = Request::post("/rpc")
        .header("Authorization", &format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .map_err(|err| format!("encode request: {err}"))?
        .send()
        .await
        .map_err(|err| format!("POST /rpc failed: {err}"))?;
    if !response.ok() {
        return Err(format!("/rpc returned HTTP {}", response.status()));
    }
    let envelope: Value = response
        .json()
        .await
        .map_err(|err| format!("decode /rpc response: {err}"))?;
    if let Some(err) = envelope.get("error") {
        return Err(format!("JSON-RPC error: {err}"));
    }
    let result = envelope
        .get("result")
        .ok_or_else(|| "response missing `result`".to_string())?
        .clone();
    serde_json::from_value(result).map_err(|err| format!("deserialize {method} result: {err}"))
}

/// Monotonic counter so each `runtime/jobs/start` carries a unique `job_id`.
static JOB_SEQ: AtomicU64 = AtomicU64::new(0);

/// A unique client-side job id (single-threaded wasm: a timestamp + counter).
fn next_job_id() -> String {
    let n = JOB_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("gui-{}-{n}", js_sys::Date::now() as u64)
}

/// Start a job carrying the `RuntimeCommand` `command` (e.g. `session.start` /
/// `session.message`); returns the JSON-RPC `result` value. The session lifecycle
/// rides the existing job machinery — no new transport.
pub async fn start_job(token: &str, command: Value) -> Result<Value, String> {
    rpc_call::<Value>(
        token,
        "runtime/jobs/start",
        json!({ "job_id": next_job_id(), "command": command }),
    )
    .await
}

/// A fresh client-side job id, exposed so a caller can install routing/session
/// state for a job BEFORE issuing the start RPC — for `delegate.start`, events
/// (`DelegateProgress` / `ApprovalRequested` / `SessionIdle`) keyed by this id
/// can arrive concurrently with, or before, the RPC round-trip resolves.
pub fn new_job_id() -> String {
    next_job_id()
}

/// Start a job under a caller-supplied `job_id` (see [`new_job_id`]); does not
/// await completion (the delegate.start job PARKS as a live session).
pub async fn start_job_with_id(token: &str, job_id: &str, command: Value) -> Result<(), String> {
    rpc_call::<Value>(
        token,
        "runtime/jobs/start",
        json!({ "job_id": job_id, "command": command }),
    )
    .await?;
    Ok(())
}

/// Request cancellation of a running job (`runtime/jobs/cancel`). Used to stop a
/// delegate turn (the turn's job id).
pub async fn cancel_job(token: &str, job_id: &str) -> Result<(), String> {
    rpc_call::<Value>(token, "runtime/jobs/cancel", json!({ "job_id": job_id })).await?;
    Ok(())
}

/// Start a job and poll `runtime/jobs/get` until it reaches a terminal state,
/// returning the job's `result` value. Used for short request/response jobs like
/// `session.start` whose payload (e.g. `session_id`) is only populated once the
/// job COMPLETES — the initial `jobs/start` response carries `result: null,
/// status: "running"`, so reading the id from it always failed.
pub async fn start_job_await(token: &str, command: Value) -> Result<Value, String> {
    let job_id = next_job_id();
    rpc_call::<Value>(
        token,
        "runtime/jobs/start",
        json!({ "job_id": job_id, "command": command }),
    )
    .await?;
    // ~72s ceiling (600 * 120ms) — generous for heavy snapshot assembly while
    // still bounding a job that never reaches a terminal state.
    for _ in 0..600 {
        let got = rpc_call::<Value>(token, "runtime/jobs/get", json!({ "job_id": job_id })).await?;
        let job = got.get("job").cloned().unwrap_or(Value::Null);
        match job.get("status").and_then(Value::as_str).unwrap_or("") {
            "completed" => return Ok(job.get("result").cloned().unwrap_or(Value::Null)),
            "failed" | "cancelled" => return Err(job_error(&job)),
            _ => gloo_timers::future::TimeoutFuture::new(120).await,
        }
    }
    Err("job did not complete in time".to_string())
}

/// Human-readable error from a terminal job (string or stringified object).
fn job_error(job: &Value) -> String {
    job.get("error")
        .filter(|e| !e.is_null())
        .map(|e| {
            e.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| e.to_string())
        })
        .unwrap_or_else(|| "job did not complete".to_string())
}

/// Open the `/events` SSE stream and invoke `on_event` for each decoded
/// [`RuntimeEvent`]. `on_status` reports the connection lifecycle (`true` on open,
/// `false` on error) so the UI can show a "reconnecting" state — the browser
/// auto-reconnects (replaying via `Last-Event-ID`), but without this the UI would
/// silently freeze during a daemon outage. The token rides the URL query (an
/// `EventSource` cannot set headers). The `EventSource` + its closures are leaked
/// deliberately: the stream lives for the app's lifetime.
pub fn open_events(
    token: &str,
    on_event: impl Fn(RuntimeEvent) + 'static,
    on_status: impl Fn(bool) + 'static,
) -> Result<(), String> {
    let url = format!("/events?token={token}");
    let source = EventSource::new(&url).map_err(|err| format!("open /events: {err:?}"))?;
    let handler = Closure::<dyn FnMut(MessageEvent)>::new(move |message: MessageEvent| {
        let Some(data) = message.data().as_string() else {
            return;
        };
        // Each frame is a JSON-RPC notification `{method:"runtime/event", params:<RuntimeEvent>}`.
        let Ok(note) = serde_json::from_str::<Value>(&data) else {
            return;
        };
        let params = note.get("params").cloned().unwrap_or(Value::Null);
        if let Ok(event) = serde_json::from_value::<RuntimeEvent>(params) {
            on_event(event);
        }
    });
    source.set_onmessage(Some(handler.as_ref().unchecked_ref()));
    handler.forget();

    // Connection lifecycle → `on_status` (open clears the "reconnecting" banner;
    // error sets it). Both closures are leaked alongside the stream.
    let on_status = Rc::new(on_status);
    let on_open = {
        let status = Rc::clone(&on_status);
        Closure::<dyn FnMut()>::new(move || status(true))
    };
    source.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    on_open.forget();
    let on_err = Closure::<dyn FnMut()>::new(move || on_status(false));
    source.set_onerror(Some(on_err.as_ref().unchecked_ref()));
    on_err.forget();

    std::mem::forget(source);
    Ok(())
}
