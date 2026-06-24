//! DA-5c: the *persistent, steerable* codex **app-server** delegate session.
//!
//! [`delegate_session`](crate::delegate_session) (DA-5a/5b) is the claude
//! equivalent over `stream-json`. This module is the codex side: a `codex
//! app-server` process kept alive across turns, driven over the app-server's
//! JSON-RPC-2.0-subset protocol (newline-delimited JSON, **not** LSP
//! Content-Length framing), so the same thread can be steered with follow-up
//! turns and codex's tool-permission asks are routed through Nerve's approval hub.
//!
//! ## The pinned codex app-server protocol (v2 thread/turn vocabulary)
//!
//! Spawn (cwd forced by the launcher):
//! ```text
//! codex -c tool_output_token_limit=<N> -c features.steer=true app-server
//! ```
//! Framing: one JSON object per line (`obj + "\n"`, single write+flush). A
//! JSON-RPC 2.0 subset with **no** `"jsonrpc"` field. Each inbound line is one of:
//! a **response** to our request (`{id,result}` / `{id,error}`), a **server→client
//! request** (`{id,method,params}` — has BOTH id and method; we must reply
//! `{id,result}`), or a **notification** (`{method,params}` — no id).
//!
//! Lifecycle:
//! 1. `initialize` request → result, then an `initialized` notification (once).
//! 2. `thread/start` request → `{thread:{id}}` — the persistent session handle.
//! 3. Per turn: `turn/start` request → `{turn:{id}}` (returns immediately; work
//!    streams as notifications), then `item/agentMessage/delta` deltas accumulate
//!    into the answer and `turn/completed` ends the turn.
//! 4. Steer = another `turn/start` on the same thread. Cancel = `turn/interrupt`.
//!
//! Approvals (the DA-5b analog): codex sends a server→client request
//! `item/commandExecution/requestApproval` (or `item/fileChange/requestApproval`);
//! we reply `{id,result:{decision:"accept"|"acceptForSession"|"decline"|"cancel"}}`.
//! See [`crate::delegate_proxy::DelegateProxy::resolve_codex`] for the mapping.
//!
//! The wire protocol (message classification, the turn accumulator, the frame
//! builders) lives in [`protocol`]; this file owns the live child + turn loop.

mod protocol;

use crate::delegate_proxy::DelegateProxy;
use crate::delegate_session::{SessionError, TurnResult, reescape_control_chars};
use crate::sandbox::{PersistentChild, SandboxLauncher};
use nerve_core::CancelToken;
use nerve_runtime::DelegateAutonomy;
use protocol::{
    Inbound, LineOutcome, TurnAccumulator, approval_result, build_codex_app_server_command,
    classify, minimal_server_reply, thread_start_params,
};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};

/// How long to wait for a turn's `turn/completed` before treating it as stalled.
/// Mirrors [`crate::delegate_session`]'s per-turn ceiling: a live session has no
/// per-command clock, but one turn must not hang a job thread forever.
const TURN_TIMEOUT: Duration = Duration::from_secs(600);

/// How long to wait for a handshake / `thread/start` / `turn/start` *response*
/// (these return promptly; the work itself streams afterwards).
const RPC_TIMEOUT: Duration = Duration::from_secs(60);

/// How often the read loop wakes to re-check cancellation while blocking on lines.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A live, steerable delegated `codex app-server` session: owns the persistent
/// child, the JSON-RPC id counter, the started thread id, and the optional
/// approval proxy.
pub(crate) struct CodexSession {
    child: PersistentChild,
    /// Monotonic JSON-RPC request id for client→server requests.
    next_id: i64,
    /// The thread id from `thread/start` — the persistent session handle every
    /// `turn/start` targets.
    thread_id: String,
    /// The confined working dir, captured at start and reused for every turn's
    /// `cwd` (so a steer needn't re-supply it — it stays on the thread's root).
    cwd: std::path::PathBuf,
    /// The id of the in-flight turn, captured from the `turn/started`/`turn/start`
    /// response and from any approval request's params. [`Self::interrupt`] targets
    /// it so a `turn/interrupt` matches the real turn rather than carrying `""` (an
    /// empty `turnId` is a no-op — real codex can't match the in-flight turn).
    current_turn_id: String,
    /// DA-5b analog: when present, the child runs proxied (approvalPolicy
    /// `untrusted` + approvalsReviewer `user`) and `requestApproval` server-requests
    /// route through this proxy. `None` keeps the autonomy-driven sandbox mode.
    proxy: Option<DelegateProxy>,
}

impl CodexSession {
    /// Spawn the persistent `codex app-server` child, run the handshake +
    /// `thread/start`, then turn 1 (forwarding agent-message text to `on_progress`).
    /// A refused spawn (no `--allow-delegate`) surfaces as [`SessionError::Io`].
    #[allow(clippy::too_many_arguments)] // reason: one cohesive spawn call; cwd,
    // autonomy, model, the first message, the optional approval proxy, and the
    // cancel/progress sinks are independent inputs to the start — bundling them
    // into a struct would add indirection without isolating a responsibility.
    pub(crate) fn start(
        launcher: &dyn SandboxLauncher,
        cwd: &Path,
        autonomy: DelegateAutonomy,
        model: Option<&str>,
        first_message: &str,
        proxy: Option<DelegateProxy>,
        mcp_disable_flags: &[String],
        cancel: &CancelToken,
        on_progress: &mut dyn FnMut(&str),
    ) -> Result<(Self, TurnResult), SessionError> {
        let spec = build_codex_app_server_command(mcp_disable_flags);
        let policy = crate::delegate_runtime::delegate_policy(cwd);
        let child = launcher
            .launch_persistent(&spec, &policy)
            .map_err(|err| SessionError::Io(err.to_string()))?;
        let mut session = Self {
            child,
            next_id: 1,
            thread_id: String::new(),
            cwd: cwd.to_path_buf(),
            current_turn_id: String::new(),
            proxy,
        };
        match session.boot(autonomy, model, first_message, cancel, on_progress) {
            Ok(turn) => Ok((session, turn)),
            // A boot/turn-1 failure must not leak the child: reap before returning.
            Err(err) => {
                session.close();
                Err(err)
            }
        }
    }

    /// Run the handshake, start the thread, and execute turn 1.
    fn boot(
        &mut self,
        autonomy: DelegateAutonomy,
        model: Option<&str>,
        first_message: &str,
        cancel: &CancelToken,
        on_progress: &mut dyn FnMut(&str),
    ) -> Result<TurnResult, SessionError> {
        self.initialize(cancel)?;
        self.start_thread(autonomy, model, cancel)?;
        self.run_turn(first_message, cancel, on_progress)
    }

    /// Steer the session: run another turn on the same thread.
    pub(crate) fn steer(
        &mut self,
        message: &str,
        cancel: &CancelToken,
        on_progress: &mut dyn FnMut(&str),
    ) -> Result<TurnResult, SessionError> {
        self.run_turn(message, cancel, on_progress)
    }

    /// The codex thread id (the persistent session handle), once `thread/start`
    /// has returned. Empty before boot completes.
    #[must_use]
    pub(crate) fn thread_id(&self) -> Option<&str> {
        (!self.thread_id.is_empty()).then_some(self.thread_id.as_str())
    }

    /// `initialize` handshake: send the request, await its response, then send the
    /// `initialized` notification. Server-requests / notifications seen while
    /// waiting are handled (an approval can't arrive yet, but it stays robust).
    fn initialize(&mut self, cancel: &CancelToken) -> Result<(), SessionError> {
        let id = self.send_request(
            "initialize",
            json!({
                "clientInfo": { "name": "nerve", "version": env!("CARGO_PKG_VERSION") },
                "capabilities": { "experimentalApi": true },
            }),
        )?;
        self.await_response(id, cancel)?;
        self.send_notification("initialized", json!({}))?;
        Ok(())
    }

    /// `thread/start`: open the persistent thread, capturing its id. In proxied
    /// mode the policy is `untrusted` + `approvalsReviewer:user` + a restrictive
    /// sandbox; otherwise the autonomy → sandbox map with `approvalPolicy:never`.
    fn start_thread(
        &mut self,
        autonomy: DelegateAutonomy,
        model: Option<&str>,
        cancel: &CancelToken,
    ) -> Result<(), SessionError> {
        let params = thread_start_params(&self.cwd, autonomy, model, self.proxy.is_some());
        let id = self.send_request("thread/start", params)?;
        let result = self.await_response(id, cancel)?;
        let thread_id = result
            .get("thread")
            .and_then(|t| t.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| SessionError::Io("thread/start returned no thread id".to_string()))?;
        self.thread_id = thread_id.to_string();
        Ok(())
    }

    /// Run one turn: `turn/start` on the thread, then drive the notification stream
    /// until `turn/completed`, forwarding agent-message text to `on_progress` and
    /// routing any `requestApproval` server-request through the proxy.
    fn run_turn(
        &mut self,
        message: &str,
        cancel: &CancelToken,
        on_progress: &mut dyn FnMut(&str),
    ) -> Result<TurnResult, SessionError> {
        let params = json!({
            "threadId": self.thread_id,
            "input": [{ "type": "text", "text": message, "text_elements": [] }],
            "cwd": self.cwd.display().to_string(),
        });
        // Start this turn with no known id; it is captured from the turn/start
        // response (or an approval request) so a cancel/interrupt targets it.
        self.current_turn_id.clear();
        let turn_req_id = self.send_request("turn/start", params)?;
        self.read_turn(turn_req_id, cancel, on_progress)
    }

    /// Block on the line stream until this turn completes. Handles the `turn/start`
    /// response (capturing the turn id), accumulates agent-message deltas, routes
    /// approvals, and finishes on `turn/completed`. Honors cancel (interrupt then
    /// [`SessionError::Cancelled`]), child exit, and the per-turn timeout.
    fn read_turn(
        &mut self,
        turn_req_id: i64,
        cancel: &CancelToken,
        on_progress: &mut dyn FnMut(&str),
    ) -> Result<TurnResult, SessionError> {
        let mut acc = TurnAccumulator::default();
        let deadline = Instant::now() + TURN_TIMEOUT;
        loop {
            if cancel.is_cancelled() {
                self.interrupt();
                return Err(SessionError::Cancelled);
            }
            match self.child.lines().recv_timeout(POLL_INTERVAL) {
                Ok(raw) => {
                    match self.ingest_line(&raw, turn_req_id, &mut acc, cancel, on_progress)? {
                        LineOutcome::Done => return Ok(acc.finish()),
                        LineOutcome::Interrupted => return Err(SessionError::Cancelled),
                        LineOutcome::Continue => {}
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if Instant::now() >= deadline {
                        return Err(SessionError::TurnTimedOut);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(SessionError::ProcessExited);
                }
            }
        }
    }

    /// Classify and handle one inbound line during a turn.
    fn ingest_line(
        &mut self,
        raw: &str,
        turn_req_id: i64,
        acc: &mut TurnAccumulator,
        cancel: &CancelToken,
        on_progress: &mut dyn FnMut(&str),
    ) -> Result<LineOutcome, SessionError> {
        // Re-escape bare control bytes so a stray <0x20 in a string value can't make
        // serde reject the line (and silently drop a delta/completion) — the claude
        // driver applies the same guard to its shared line source.
        let line = reescape_control_chars(raw);
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            return Ok(LineOutcome::Continue);
        };
        match classify(&value) {
            Inbound::Response { id } => {
                // The turn/start response carries the turn id; other responses
                // (none expected mid-turn) are ignored.
                if id == turn_req_id {
                    // A turn/start that itself errored must fail fast rather than
                    // looping to the per-turn timeout.
                    if let Some(error) = value.get("error") {
                        return Err(SessionError::Io(format!("codex turn/start error: {error}")));
                    }
                    acc.capture_turn_id(&value);
                    self.capture_turn_id(&acc.turn_id);
                }
                Ok(LineOutcome::Continue)
            }
            Inbound::ServerRequest { id, method } => {
                Ok(self.handle_server_request(id, &method, &value, cancel))
            }
            Inbound::Notification { method } => {
                Ok(acc.ingest_notification(&method, &value, on_progress))
            }
            Inbound::Unknown => Ok(LineOutcome::Continue),
        }
    }

    /// Record the in-flight turn id (from the turn/start response or an approval
    /// request's params) so [`Self::interrupt`] targets the real turn. Ignores an
    /// empty id so a later concrete id is not overwritten by a blank one.
    fn capture_turn_id(&mut self, turn_id: &str) {
        if !turn_id.is_empty() {
            self.current_turn_id = turn_id.to_string();
        }
    }

    /// Handle a server→client request: a `requestApproval` is routed through the
    /// proxy and answered `{id,result:{decision}}`; a deny-under-cancel also
    /// interrupts the turn. Anything else gets a minimal safe reply.
    fn handle_server_request(
        &mut self,
        id: i64,
        method: &str,
        request: &Value,
        cancel: &CancelToken,
    ) -> LineOutcome {
        if method.ends_with("/requestApproval") {
            return self.handle_approval(id, method, request, cancel);
        }
        // Unknown server-requests (e.g. `item/tool/requestUserInput`) get a minimal
        // decline-shaped reply so the server isn't left waiting.
        let _ = self.reply(id, minimal_server_reply(method));
        LineOutcome::Continue
    }

    /// Route a `requestApproval` through the proxy and reply with the decision.
    fn handle_approval(
        &mut self,
        id: i64,
        method: &str,
        request: &Value,
        cancel: &CancelToken,
    ) -> LineOutcome {
        // The approval request names the turn it belongs to; capture it so a
        // deny-under-cancel interrupt targets the real turn even if the approval
        // arrived before the turn/start response was processed.
        if let Some(turn_id) = request
            .get("params")
            .and_then(|p| p.get("turnId"))
            .and_then(Value::as_str)
        {
            self.capture_turn_id(turn_id);
        }
        let Some(proxy) = self.proxy.as_ref() else {
            // No approver wired (autonomy mode) — codex shouldn't ask, but if it
            // does, decline safely rather than blocking.
            let _ = self.reply(id, approval_result(method, "decline"));
            return LineOutcome::Continue;
        };
        let response = proxy.resolve_codex(request, cancel);
        let _ = self.reply(id, approval_result(method, &response.decision));
        if response.interrupted {
            self.interrupt();
            return LineOutcome::Interrupted;
        }
        LineOutcome::Continue
    }

    /// Send `turn/interrupt` for the in-flight turn (best effort). Targets the
    /// captured `current_turn_id` so real codex can match the running turn; a blank
    /// id (no turn started yet) is a no-op on codex's side either way.
    pub(crate) fn interrupt(&self) {
        let frame = json!({
            "id": -1,
            "method": "turn/interrupt",
            "params": { "threadId": self.thread_id, "turnId": self.current_turn_id },
        });
        let _ = self.child.write_line(&format!("{frame}\n"));
    }

    /// End the session: close stdin (→ EOF graceful exit) and reap; force-kill the
    /// process group if it does not exit promptly. Mirrors
    /// [`crate::delegate_session::DelegateSession::close`].
    pub(crate) fn close(&mut self) {
        self.child.close_stdin();
        for _ in 0..50 {
            if self.child.has_exited() {
                break;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        if !self.child.has_exited() {
            self.child.kill();
        }
        let _ = self.child.wait();
    }

    // ---- JSON-RPC plumbing --------------------------------------------------

    /// Send a client→server request, returning its id. The id counter is bumped so
    /// each request is uniquely answerable.
    fn send_request(&mut self, method: &str, params: Value) -> Result<i64, SessionError> {
        let id = self.next_id;
        self.next_id += 1;
        let frame = json!({ "id": id, "method": method, "params": params });
        self.write(&frame)?;
        Ok(id)
    }

    /// Send a client→server notification (no id, no response expected).
    fn send_notification(&self, method: &str, params: Value) -> Result<(), SessionError> {
        self.write(&json!({ "method": method, "params": params }))
    }

    /// Reply to a server→client request with `{id,result}`.
    fn reply(&self, id: i64, result: Value) -> Result<(), SessionError> {
        self.write(&json!({ "id": id, "result": result }))
    }

    /// Write one JSON value as a single framed line (object + `\n`, one flush).
    fn write(&self, value: &Value) -> Result<(), SessionError> {
        self.child
            .write_line(&format!("{value}\n"))
            .map_err(|err| SessionError::Io(err.to_string()))
    }

    /// Block until the response to `id` arrives, handling intervening
    /// server-requests / notifications. Used for the prompt-returning calls
    /// (`initialize`, `thread/start`).
    fn await_response(&mut self, id: i64, cancel: &CancelToken) -> Result<Value, SessionError> {
        let deadline = Instant::now() + RPC_TIMEOUT;
        loop {
            if cancel.is_cancelled() {
                return Err(SessionError::Cancelled);
            }
            match self.child.lines().recv_timeout(POLL_INTERVAL) {
                Ok(raw) => {
                    if let Some(result) = self.handle_pre_turn_line(&raw, id, cancel)? {
                        return Ok(result);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if Instant::now() >= deadline {
                        return Err(SessionError::TurnTimedOut);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(SessionError::ProcessExited);
                }
            }
        }
    }

    /// Handle one line seen while awaiting a handshake response: return `Some(result)`
    /// when it is the awaited response, else handle server-requests and keep waiting.
    fn handle_pre_turn_line(
        &mut self,
        raw: &str,
        awaited: i64,
        cancel: &CancelToken,
    ) -> Result<Option<Value>, SessionError> {
        // Same control-byte re-escaping as the in-turn parse site, so a malformed
        // handshake line can't be silently dropped (which would stall read_turn).
        let line = reescape_control_chars(raw);
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            return Ok(None);
        };
        match classify(&value) {
            Inbound::Response { id } if id == awaited => {
                if let Some(error) = value.get("error") {
                    return Err(SessionError::Io(format!("codex rpc error: {error}")));
                }
                Ok(Some(value.get("result").cloned().unwrap_or(Value::Null)))
            }
            Inbound::ServerRequest { id, method } => {
                self.handle_server_request(id, &method, &value, cancel);
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Finding 10: a codex `item/agentMessage/delta` line carrying a bare control
    /// byte inside its string value must NOT be silently dropped. The raw line fails
    /// `serde_json::from_str`, but after `reescape_control_chars` it parses and the
    /// delta is accumulated — exactly what the wrapped parse site in `ingest_line`
    /// now does (this exercises that classify+ingest path on the re-escaped line).
    #[test]
    fn control_byte_in_codex_delta_is_reescaped_not_dropped() {
        // A literal U+0001 inside the delta string. `serde_json` rejects a bare
        // control byte in a string, so without re-escaping the line is dropped.
        let raw =
            "{\"method\":\"item/agentMessage/delta\",\"params\":{\"delta\":\"hi\u{0001}there\"}}";
        assert!(
            serde_json::from_str::<serde_json::Value>(raw).is_err(),
            "the raw control-byte line should be rejected before re-escaping"
        );

        let line = reescape_control_chars(raw);
        let value: serde_json::Value =
            serde_json::from_str(&line).expect("re-escaped line should parse");

        let mut acc = TurnAccumulator::default();
        let mut streamed = String::new();
        let mut on_progress = |t: &str| streamed.push_str(t);
        let outcome = match classify(&value) {
            Inbound::Notification { method } => {
                acc.ingest_notification(&method, &value, &mut on_progress)
            }
            other => panic!("expected a notification, got {other:?}"),
        };
        assert!(matches!(outcome, LineOutcome::Continue));
        // The delta was accumulated (with the control byte preserved as an escape),
        // not dropped: the assistant text survives instead of being lost.
        assert_eq!(streamed, "hi\u{0001}there");
    }
}
