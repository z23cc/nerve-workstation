//! DA-5b: routing a delegated `claude` session's tool-permission prompts through
//! Nerve's own approval system.
//!
//! [`delegate_session`](crate::delegate_session) (DA-5a) runs a persistent claude
//! child driven by `--permission-mode <plan|acceptEdits|bypassPermissions>`, where
//! claude decides tool permissions itself. This module adds **proxied mode**: when
//! an [`ApprovalHub`](crate::session_manager::ApprovalHub)-backed approver is
//! available, the child is started with `--permission-prompt-tool stdio
//! --permission-mode default`, so claude *asks before each tool use* and Nerve's
//! operator approves (via the same approval modal that gates Nerve's own tools).
//!
//! ## The pinned claude permission protocol (verified, live)
//!
//! When a tool needs approval, claude emits on stdout (the turn blocks until a
//! response is written back):
//! ```json
//! {"type":"control_request","request_id":"<uuid>","request":{
//!     "subtype":"can_use_tool","tool_name":"Bash","input":{...},
//!     "permission_suggestions":[...],"tool_use_id":"toolu_...","blocked_path":"..."}}
//! ```
//! The reply is one framed write — **outer** envelope snake_case, **inner**
//! decision camelCase:
//! ```json
//! ALLOW: {"type":"control_response","response":{"subtype":"success",
//!   "request_id":"<same>","response":{"behavior":"allow",
//!   "updatedInput":<echo input>,"toolUseID":"<tool_use_id>"}}}
//! DENY:  {"type":"control_response","response":{"subtype":"success",
//!   "request_id":"<same>","response":{"behavior":"deny","message":"<reason>"}}}
//! ```
//! A deny that also cancels the whole turn adds `"interrupt":true` to the deny
//! inner object. A `control_cancel_request` (claude withdrew a pending ask) drops
//! the pending approval; a `keep_alive` is ignored.

use nerve_core::CancelToken;
use nerve_runtime::{RiskTier, SessionApprovalDecision};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Per-delegate-session memory of remembered approval decisions: tool name →
/// `true` (allow-always) / `false` (deny-always). Mirrors the session agent's
/// `DecisionMemory` so an `AllowAlways` / `DenyAlways` persists across `can_use_tool`
/// asks for the life of the delegated session, and a deny-always never re-prompts
/// (a claude that keeps re-requesting a refused tool can't wedge the operator).
pub(crate) type DelegateDecisions = Arc<Mutex<HashMap<String, bool>>>;

/// The approval seam the delegated session calls into. Implemented by the
/// session-manager's `ApprovalHub`, so a `can_use_tool` ask emits an
/// `approval_requested` event (the TUI modal) and blocks for the operator's
/// `session.respond` — the exact round-trip Nerve's own tools use. Kept as a trait
/// so [`delegate_session`](crate::delegate_session) does not depend on
/// session-manager internals.
pub(crate) trait DelegateApprover: Send + Sync {
    /// Emit an `approval_requested` for `tool` (with `args`, `tier`, `preview`)
    /// under `session_id` and block for the operator's decision. Cancellation /
    /// timeout resolve to [`SessionApprovalDecision::Deny`].
    fn request(
        &self,
        session_id: &str,
        tool: &str,
        args: &Value,
        tier: RiskTier,
        preview: String,
        cancel: &CancelToken,
    ) -> SessionApprovalDecision;
}

/// Proxied-mode permission state for a live delegated session: the approver to
/// route `can_use_tool` asks to, the delegate session id (== the start job id) the
/// approval is keyed under, and the per-session remembered allow/deny decisions.
pub(crate) struct DelegateProxy {
    approver: Arc<dyn DelegateApprover>,
    session_id: String,
    decisions: DelegateDecisions,
    /// The catalog agent name (`claude` / `codex`) this proxy serves. Selects the
    /// tool-tier classifier and names the agent in the approval preview, so a codex
    /// ask reads "codex wants to run …" and an exec/file-change tier is right per
    /// agent. Defaults to `claude` for [`Self::new`] (the DA-5b constructor).
    agent: String,
}

/// What the reader should do after handling a `can_use_tool` ask: write the built
/// `control_response` line, and (on a deny+interrupt) treat the turn as cancelled.
pub(crate) struct ProxyResponse {
    /// The framed `control_response` line (no trailing newline) to write on stdin.
    pub(crate) line: String,
    /// Whether the response interrupted the turn (deny while the session's cancel
    /// token fired) — the caller writes the line then ends the turn as cancelled.
    pub(crate) interrupted: bool,
}

/// DA-5c: the codex analog of [`ProxyResponse`]. codex's approval reply is the
/// `result` body of a JSON-RPC response (`{id,result:{decision}}`), so this carries
/// only the mapped `decision` string (`accept` / `acceptForSession` / `decline` /
/// `cancel`) — the caller frames the `{id,result}` envelope. `interrupted` mirrors
/// [`ProxyResponse`]: a deny under cancel both replies `cancel` and ends the turn.
pub(crate) struct CodexProxyResponse {
    /// The `decision` value for the reply's `result` object.
    pub(crate) decision: String,
    /// Whether the response also interrupts the turn (a deny while cancelled).
    pub(crate) interrupted: bool,
}

impl DelegateProxy {
    /// DA-5c: construct a proxy for a specific agent (`claude` / `codex`), selecting
    /// the per-agent tool-tier classifier and preview label.
    pub(crate) fn for_agent(
        approver: Arc<dyn DelegateApprover>,
        session_id: String,
        decisions: DelegateDecisions,
        agent: &str,
    ) -> Self {
        Self {
            approver,
            session_id,
            decisions,
            agent: agent.to_string(),
        }
    }

    /// Resolve a `can_use_tool` control_request into a `control_response` line.
    ///
    /// A remembered decision short-circuits without a fresh prompt (allow-always →
    /// allow, deny-always → deny). Otherwise the approval hub is consulted (which
    /// blocks the reader thread — acceptable: the claude turn is itself blocked on
    /// the response). A remembered allow/deny is recorded so repeats skip the
    /// prompt; a one-shot Allow/Deny applies to this call only. A deny that is
    /// really a cancel (the session token fired) interrupts the whole turn.
    pub(crate) fn resolve(&self, request: &Value, cancel: &CancelToken) -> ProxyResponse {
        let request_id = string_field(request, "request_id");
        let inner = request.get("request");
        let tool = inner
            .and_then(|r| r.get("tool_name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let input = inner
            .and_then(|r| r.get("input"))
            .cloned()
            .unwrap_or(Value::Null);
        let tool_use_id = inner
            .and_then(|r| r.get("tool_use_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let decision = self.decide(&tool, &input, cancel);
        if decision.allows() {
            return ProxyResponse {
                line: allow_response(&request_id, &input, &tool_use_id),
                interrupted: false,
            };
        }
        // A deny issued because the session was cancelled also interrupts the turn,
        // so claude tears the turn down rather than continuing after the refusal.
        let interrupt = cancel.is_cancelled();
        ProxyResponse {
            line: deny_response(&request_id, deny_message(&decision), interrupt),
            interrupted: interrupt,
        }
    }

    /// DA-5c: resolve a codex app-server `*/requestApproval` server-request into the
    /// `{decision}` payload of the JSON-RPC reply. Unlike [`Self::resolve`] (claude's
    /// stdout `can_use_tool` → `control_response`), codex sends a JSON-RPC *request*
    /// (it has an `id` the caller echoes); this builds only the `result` body, and the
    /// caller frames `{id,result}`. The same [`Self::decide`] machinery is reused, so
    /// allow-always / deny-always memory and the operator round-trip are identical;
    /// the decision is mapped to codex's `accept` / `acceptForSession` / `decline`
    /// vocabulary. A deny under cancel sets `interrupted` so the caller also tears the
    /// turn down (codex's `cancel` reply).
    pub(crate) fn resolve_codex(
        &self,
        request: &Value,
        cancel: &CancelToken,
    ) -> CodexProxyResponse {
        let (tool, input) = codex_tool_and_input(request);
        let decision = self.decide(&tool, &input, cancel);
        let interrupted = !decision.allows() && cancel.is_cancelled();
        CodexProxyResponse {
            decision: codex_decision(&decision, interrupted),
            interrupted,
        }
    }

    /// Decide the approval for `tool`: a remembered decision wins; otherwise prompt
    /// the operator and record an `AllowAlways` / `DenyAlways` for future asks.
    fn decide(&self, tool: &str, input: &Value, cancel: &CancelToken) -> SessionApprovalDecision {
        if let Some(&allow) = crate::sync::lock_recover(&self.decisions).get(tool) {
            return if allow {
                SessionApprovalDecision::Allow
            } else {
                SessionApprovalDecision::Deny
            };
        }
        let tier = if self.agent == "codex" {
            codex_tool_tier(tool)
        } else {
            claude_tool_tier(tool)
        };
        let preview = delegate_preview(&self.agent, tool, input);
        let decision = self
            .approver
            .request(&self.session_id, tool, input, tier, preview, cancel);
        // Persist a remembered allow/deny so future asks for this tool skip the
        // prompt; don't persist a deny that is really a turn interrupt.
        if decision.remember() && !cancel.is_cancelled() {
            crate::sync::lock_recover(&self.decisions).insert(tool.to_string(), decision.allows());
        }
        decision
    }
}

/// Risk tier for a claude tool name. claude's tool vocabulary differs from Nerve's
/// (`Bash`/`Edit`/`Read`/…), so it is classified here rather than through the
/// Nerve-keyed [`crate::policy::tool_tier`]. Fail-safe: an unknown tool (a plugin
/// or a newly added one) classifies as [`RiskTier::Exec`], the most restricted
/// tier, so it is gated rather than silently treated as benign.
fn claude_tool_tier(tool: &str) -> RiskTier {
    match tool {
        // Reads / navigation — no mutation, no exec.
        "Read" | "Glob" | "Grep" | "NotebookRead" | "TodoWrite" | "WebFetch" | "WebSearch" => {
            RiskTier::ReadOnly
        }
        // File mutation within the workspace.
        "Edit" | "Write" | "NotebookEdit" | "MultiEdit" => RiskTier::Edit,
        // Command execution and everything unrecognised (fail-safe).
        _ => RiskTier::Exec,
    }
}

/// The salient argument for a claude tool call, surfaced in the approval preview:
/// the command for `Bash`, the path for the file tools, the pattern/query for the
/// search tools, the URL for `WebFetch`. Falls back to a compact JSON dump.
fn claude_tool_summary(tool: &str, input: &Value) -> String {
    let field = match tool {
        "Bash" => "command",
        "Edit" | "Write" | "Read" | "NotebookEdit" | "MultiEdit" => "file_path",
        "Glob" | "Grep" => "pattern",
        "WebFetch" => "url",
        "WebSearch" => "query",
        _ => "",
    };
    if let Some(value) = input.get(field).and_then(Value::as_str) {
        return value.to_string();
    }
    if input.is_null() {
        String::new()
    } else {
        input.to_string()
    }
}

/// Delegate-aware approval preview: "<agent> wants to run <tool>: <summary>", where
/// the summary is the tool's salient argument (the command / path / query), so the
/// modal reads naturally for a delegated tool call rather than a raw JSON dump.
fn delegate_preview(agent: &str, tool: &str, input: &Value) -> String {
    const MAX: usize = 500;
    let summary = claude_tool_summary(tool, input);
    let rendered = if summary.is_empty() {
        format!("{agent} wants to run {tool}")
    } else {
        format!("{agent} wants to run {tool}: {summary}")
    };
    truncate_chars(&rendered, MAX)
}

/// Truncate to at most `max` characters, appending an ellipsis when cut (mirrors
/// [`crate::policy`]'s preview truncation so a long command can't bloat the modal).
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let head: String = text.chars().take(max).collect();
        format!("{head}\u{2026}")
    }
}

/// The human-readable deny reason claude surfaces to its model. A `DenyAlways`
/// notes the standing refusal so the model stops re-requesting the tool.
fn deny_message(decision: &SessionApprovalDecision) -> &'static str {
    match decision {
        SessionApprovalDecision::DenyAlways => {
            "denied by the operator (this tool is blocked for the session)"
        }
        _ => "denied by the operator",
    }
}

/// Build the `allow` control_response: outer envelope snake_case, inner decision
/// camelCase. `updatedInput` echoes claude's requested input verbatim (Nerve does
/// not rewrite tool inputs); `toolUseID` echoes the request's `tool_use_id`.
pub(crate) fn allow_response(request_id: &str, input: &Value, tool_use_id: &str) -> String {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {
                "behavior": "allow",
                "updatedInput": input,
                "toolUseID": tool_use_id,
            },
        },
    })
    .to_string()
}

/// Build the `deny` control_response. With `interrupt` set, the inner object also
/// carries `"interrupt":true` so claude cancels the whole turn rather than letting
/// the model continue after the refusal.
pub(crate) fn deny_response(request_id: &str, message: &str, interrupt: bool) -> String {
    let mut inner = json!({ "behavior": "deny", "message": message });
    if interrupt {
        inner["interrupt"] = json!(true);
    }
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": inner,
        },
    })
    .to_string()
}

/// Read a string-valued top-level `field` from `value`, or `""` if absent.
fn string_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

// ---- DA-5c: codex app-server approval mapping --------------------------------

/// Map a Nerve [`SessionApprovalDecision`] to codex's approval vocabulary. A plain
/// allow → `accept`; an allow-always → `acceptForSession` (codex remembers it for
/// the thread); a deny → `decline`, except a deny under cancel → `cancel` so codex
/// also aborts the turn rather than just skipping the one tool call.
fn codex_decision(decision: &SessionApprovalDecision, interrupted: bool) -> String {
    if interrupted {
        return "cancel".to_string();
    }
    match decision {
        SessionApprovalDecision::Allow => "accept",
        SessionApprovalDecision::AllowAlways => "acceptForSession",
        SessionApprovalDecision::Deny | SessionApprovalDecision::DenyAlways => "decline",
    }
    .to_string()
}

/// Derive the (tool-name, input) pair for a codex `*/requestApproval` server-request,
/// so the approval modal reads naturally and the per-session decision memory keys on
/// the right family. `item/commandExecution/requestApproval` → a `Bash`-like exec
/// (the `command` is the salient arg); `item/fileChange/requestApproval` → an `Edit`
/// (a file mutation). Anything else maps to a generic tool named after the method's
/// item segment, fail-safe to the exec tier via [`crate::delegate_proxy::codex_tool_tier`].
fn codex_tool_and_input(request: &Value) -> (String, Value) {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "item/commandExecution/requestApproval" => {
            let command = codex_command_text(&params);
            (
                "Bash".to_string(),
                json!({ "command": command, "cwd": params.get("cwd").cloned() }),
            )
        }
        "item/fileChange/requestApproval" => (
            "Edit".to_string(),
            json!({ "file_path": codex_change_path(&params) }),
        ),
        other => (codex_method_label(other), params),
    }
}

/// The command string a codex exec approval is asking about. codex sends `command`
/// either as a string or an argv array; both render to a single line for the modal.
fn codex_command_text(params: &Value) -> String {
    match params.get("command") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// The path a codex file-change approval touches, for the preview. Tries the common
/// `path` field, falling back to the first entry of a `changes`/`files` list.
fn codex_change_path(params: &Value) -> String {
    if let Some(path) = params.get("path").and_then(Value::as_str) {
        return path.to_string();
    }
    for key in ["changes", "files"] {
        if let Some(first) = params
            .get(key)
            .and_then(Value::as_array)
            .and_then(|a| a.first())
        {
            if let Some(path) = first.as_str() {
                return path.to_string();
            }
            if let Some(path) = first.get("path").and_then(Value::as_str) {
                return path.to_string();
            }
        }
    }
    String::new()
}

/// A readable tool label for an unrecognised codex `*/requestApproval` method (e.g.
/// `item/permissions/requestApproval` → `permissions`), so the modal names the ask.
fn codex_method_label(method: &str) -> String {
    method
        .strip_suffix("/requestApproval")
        .and_then(|s| s.rsplit('/').next())
        .filter(|s| !s.is_empty())
        .unwrap_or("codex_tool")
        .to_string()
}

/// Risk tier for a derived codex approval tool. `Edit` is a file mutation; `Bash`
/// and everything else (an exec or an unrecognised ask) fail safe to the top tier,
/// matching [`claude_tool_tier`]'s fail-safe posture.
fn codex_tool_tier(tool: &str) -> RiskTier {
    match tool {
        "Edit" | "Write" => RiskTier::Edit,
        _ => RiskTier::Exec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_response_echoes_input_and_tool_use_id() {
        let input = json!({ "command": "ls -la" });
        let line = allow_response("req-1", &input, "toolu_abc");
        let value: Value = serde_json::from_str(&line).expect("valid json");
        // Outer envelope is snake_case.
        assert_eq!(value["type"], "control_response");
        assert_eq!(value["response"]["subtype"], "success");
        assert_eq!(value["response"]["request_id"], "req-1");
        // Inner decision is camelCase.
        let inner = &value["response"]["response"];
        assert_eq!(inner["behavior"], "allow");
        assert_eq!(inner["updatedInput"], input);
        assert_eq!(inner["toolUseID"], "toolu_abc");
    }

    #[test]
    fn deny_response_without_interrupt_omits_the_flag() {
        let line = deny_response("req-2", "denied by the operator", false);
        let value: Value = serde_json::from_str(&line).expect("valid json");
        assert_eq!(value["type"], "control_response");
        assert_eq!(value["response"]["subtype"], "success");
        assert_eq!(value["response"]["request_id"], "req-2");
        let inner = &value["response"]["response"];
        assert_eq!(inner["behavior"], "deny");
        assert_eq!(inner["message"], "denied by the operator");
        assert!(inner.get("interrupt").is_none());
    }

    #[test]
    fn deny_response_with_interrupt_sets_the_flag() {
        let line = deny_response("req-3", "denied by the operator", true);
        let value: Value = serde_json::from_str(&line).expect("valid json");
        assert_eq!(value["response"]["response"]["interrupt"], true);
        assert_eq!(value["response"]["response"]["behavior"], "deny");
    }

    /// A scripted approver returning a fixed decision, standing in for the hub.
    struct FixedApprover(SessionApprovalDecision);

    impl DelegateApprover for FixedApprover {
        fn request(
            &self,
            _session_id: &str,
            _tool: &str,
            _args: &Value,
            _tier: RiskTier,
            _preview: String,
            _cancel: &CancelToken,
        ) -> SessionApprovalDecision {
            self.0
        }
    }

    fn can_use_tool(request_id: &str, tool: &str, input: Value, tool_use_id: &str) -> Value {
        json!({
            "type": "control_request",
            "request_id": request_id,
            "request": {
                "subtype": "can_use_tool",
                "tool_name": tool,
                "input": input,
                "tool_use_id": tool_use_id,
            },
        })
    }

    fn proxy(decision: SessionApprovalDecision) -> DelegateProxy {
        DelegateProxy::for_agent(
            Arc::new(FixedApprover(decision)),
            "sess-1".to_string(),
            DelegateDecisions::default(),
            "claude",
        )
    }

    #[test]
    fn resolve_allow_builds_allow_response_echoing_request() {
        let proxy = proxy(SessionApprovalDecision::Allow);
        let request = can_use_tool("r1", "Bash", json!({ "command": "echo hi" }), "toolu_1");
        let resp = proxy.resolve(&request, &CancelToken::never());
        assert!(!resp.interrupted);
        let value: Value = serde_json::from_str(&resp.line).expect("json");
        let inner = &value["response"]["response"];
        assert_eq!(inner["behavior"], "allow");
        assert_eq!(inner["updatedInput"], json!({ "command": "echo hi" }));
        assert_eq!(inner["toolUseID"], "toolu_1");
        assert_eq!(value["response"]["request_id"], "r1");
    }

    #[test]
    fn resolve_deny_builds_deny_response_without_interrupt() {
        let proxy = proxy(SessionApprovalDecision::Deny);
        let request = can_use_tool("r2", "Edit", json!({ "path": "x" }), "toolu_2");
        let resp = proxy.resolve(&request, &CancelToken::never());
        assert!(!resp.interrupted);
        let value: Value = serde_json::from_str(&resp.line).expect("json");
        assert_eq!(value["response"]["response"]["behavior"], "deny");
        assert!(value["response"]["response"].get("interrupt").is_none());
    }

    #[test]
    fn deny_under_cancel_interrupts_the_turn() {
        let proxy = proxy(SessionApprovalDecision::Deny);
        let cancel = CancelToken::new();
        cancel.cancel();
        let request = can_use_tool("r3", "Bash", json!({}), "toolu_3");
        let resp = proxy.resolve(&request, &cancel);
        assert!(resp.interrupted);
        let value: Value = serde_json::from_str(&resp.line).expect("json");
        assert_eq!(value["response"]["response"]["interrupt"], true);
    }

    #[test]
    fn allow_always_is_remembered_and_skips_the_second_prompt() {
        // An approver that records how many times it was consulted.
        struct CountingApprover {
            calls: Arc<std::sync::atomic::AtomicUsize>,
        }
        impl DelegateApprover for CountingApprover {
            fn request(
                &self,
                _session_id: &str,
                _tool: &str,
                _args: &Value,
                _tier: RiskTier,
                _preview: String,
                _cancel: &CancelToken,
            ) -> SessionApprovalDecision {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                SessionApprovalDecision::AllowAlways
            }
        }
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let proxy = DelegateProxy::for_agent(
            Arc::new(CountingApprover {
                calls: Arc::clone(&calls),
            }),
            "sess-1".to_string(),
            DelegateDecisions::default(),
            "claude",
        );
        let cancel = CancelToken::never();
        let first = proxy.resolve(&can_use_tool("r1", "Bash", json!({}), "t1"), &cancel);
        let second = proxy.resolve(&can_use_tool("r2", "Bash", json!({}), "t2"), &cancel);
        // Both allowed, but the operator was only consulted once (the second was
        // served from the remembered allow-always).
        assert_eq!(
            serde_json::from_str::<Value>(&first.line).unwrap()["response"]["response"]["behavior"],
            "allow"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&second.line).unwrap()["response"]["response"]["behavior"],
            "allow"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn deny_always_is_remembered_and_auto_denies_repeats() {
        struct CountingApprover {
            calls: Arc<std::sync::atomic::AtomicUsize>,
        }
        impl DelegateApprover for CountingApprover {
            fn request(
                &self,
                _session_id: &str,
                _tool: &str,
                _args: &Value,
                _tier: RiskTier,
                _preview: String,
                _cancel: &CancelToken,
            ) -> SessionApprovalDecision {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                SessionApprovalDecision::DenyAlways
            }
        }
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let proxy = DelegateProxy::for_agent(
            Arc::new(CountingApprover {
                calls: Arc::clone(&calls),
            }),
            "sess-1".to_string(),
            DelegateDecisions::default(),
            "claude",
        );
        let cancel = CancelToken::never();
        let first = proxy.resolve(&can_use_tool("r1", "Bash", json!({}), "t1"), &cancel);
        let second = proxy.resolve(&can_use_tool("r2", "Bash", json!({}), "t2"), &cancel);
        assert_eq!(
            serde_json::from_str::<Value>(&first.line).unwrap()["response"]["response"]["behavior"],
            "deny"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&second.line).unwrap()["response"]["response"]["behavior"],
            "deny"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn delegate_preview_reads_naturally() {
        assert_eq!(
            delegate_preview("claude", "Bash", &json!({ "command": "ls" })),
            "claude wants to run Bash: ls"
        );
        assert_eq!(
            delegate_preview("claude", "Edit", &json!({ "file_path": "src/main.rs" })),
            "claude wants to run Edit: src/main.rs"
        );
        // The agent label flows through, so a codex ask reads as codex.
        assert_eq!(
            delegate_preview("codex", "Bash", &json!({ "command": "ls" })),
            "codex wants to run Bash: ls"
        );
        // A tool with no salient field still names the tool.
        assert_eq!(
            delegate_preview("claude", "UnknownTool", &Value::Null),
            "claude wants to run UnknownTool"
        );
        // Overlong previews are truncated with an ellipsis.
        let long = "x".repeat(600);
        let preview = delegate_preview("claude", "Bash", &json!({ "command": long }));
        assert_eq!(preview.chars().count(), 501);
        assert!(preview.ends_with('\u{2026}'));
    }

    #[test]
    fn claude_tool_tier_classifies_and_fails_safe() {
        for tool in ["Read", "Glob", "Grep", "WebFetch", "WebSearch"] {
            assert_eq!(claude_tool_tier(tool), RiskTier::ReadOnly, "{tool}");
        }
        for tool in ["Edit", "Write", "NotebookEdit", "MultiEdit"] {
            assert_eq!(claude_tool_tier(tool), RiskTier::Edit, "{tool}");
        }
        // Bash and any unknown / plugin tool fail safe to the top tier.
        for tool in ["Bash", "Task", "mcp__x__y", "BrandNewTool"] {
            assert_eq!(claude_tool_tier(tool), RiskTier::Exec, "{tool}");
        }
    }

    // ---- DA-5c: codex approval mapping --------------------------------------

    /// Build a codex `*/requestApproval` server-request envelope (id + method +
    /// params) like the app-server sends.
    fn codex_request(id: i64, method: &str, params: Value) -> Value {
        json!({ "id": id, "method": method, "params": params })
    }

    fn codex_proxy(decision: SessionApprovalDecision) -> DelegateProxy {
        DelegateProxy::for_agent(
            Arc::new(FixedApprover(decision)),
            "sess-codex".to_string(),
            DelegateDecisions::default(),
            "codex",
        )
    }

    #[test]
    fn codex_decision_maps_nerve_to_codex_vocabulary() {
        assert_eq!(
            codex_decision(&SessionApprovalDecision::Allow, false),
            "accept"
        );
        assert_eq!(
            codex_decision(&SessionApprovalDecision::AllowAlways, false),
            "acceptForSession"
        );
        assert_eq!(
            codex_decision(&SessionApprovalDecision::Deny, false),
            "decline"
        );
        assert_eq!(
            codex_decision(&SessionApprovalDecision::DenyAlways, false),
            "decline"
        );
        // A deny under interrupt → cancel (aborts the turn, not just the one tool).
        assert_eq!(
            codex_decision(&SessionApprovalDecision::Deny, true),
            "cancel"
        );
    }

    #[test]
    fn codex_tool_and_input_classifies_exec_and_file_change() {
        let (tool, input) = codex_tool_and_input(&codex_request(
            7,
            "item/commandExecution/requestApproval",
            json!({ "command": ["echo", "hi"], "cwd": "/w" }),
        ));
        assert_eq!(tool, "Bash");
        assert_eq!(input["command"], "echo hi");
        assert_eq!(codex_tool_tier(&tool), RiskTier::Exec);

        let (tool, input) = codex_tool_and_input(&codex_request(
            8,
            "item/fileChange/requestApproval",
            json!({ "path": "src/main.rs" }),
        ));
        assert_eq!(tool, "Edit");
        assert_eq!(input["file_path"], "src/main.rs");
        assert_eq!(codex_tool_tier(&tool), RiskTier::Edit);

        // An unrecognised approval method gets a readable label + the safe tier.
        let (tool, _) = codex_tool_and_input(&codex_request(
            9,
            "item/permissions/requestApproval",
            json!({}),
        ));
        assert_eq!(tool, "permissions");
        assert_eq!(codex_tool_tier(&tool), RiskTier::Exec);
    }

    #[test]
    fn codex_change_path_reads_list_shapes() {
        assert_eq!(
            codex_change_path(&json!({ "changes": [{ "path": "a.rs" }] })),
            "a.rs"
        );
        assert_eq!(codex_change_path(&json!({ "files": ["b.rs"] })), "b.rs");
        assert_eq!(codex_change_path(&json!({})), "");
    }

    #[test]
    fn resolve_codex_allow_maps_to_accept() {
        let proxy = codex_proxy(SessionApprovalDecision::Allow);
        let resp = proxy.resolve_codex(
            &codex_request(
                1,
                "item/commandExecution/requestApproval",
                json!({ "command": "echo hi" }),
            ),
            &CancelToken::never(),
        );
        assert!(!resp.interrupted);
        assert_eq!(resp.decision, "accept");
    }

    #[test]
    fn resolve_codex_deny_maps_to_decline_no_interrupt() {
        let proxy = codex_proxy(SessionApprovalDecision::Deny);
        let resp = proxy.resolve_codex(
            &codex_request(2, "item/fileChange/requestApproval", json!({ "path": "x" })),
            &CancelToken::never(),
        );
        assert!(!resp.interrupted);
        assert_eq!(resp.decision, "decline");
    }

    #[test]
    fn resolve_codex_deny_under_cancel_maps_to_cancel_and_interrupts() {
        let proxy = codex_proxy(SessionApprovalDecision::Deny);
        let cancel = CancelToken::new();
        cancel.cancel();
        let resp = proxy.resolve_codex(
            &codex_request(3, "item/commandExecution/requestApproval", json!({})),
            &cancel,
        );
        assert!(resp.interrupted);
        assert_eq!(resp.decision, "cancel");
    }

    #[test]
    fn resolve_codex_allow_always_remembers_and_skips_second_prompt() {
        struct CountingApprover {
            calls: Arc<std::sync::atomic::AtomicUsize>,
        }
        impl DelegateApprover for CountingApprover {
            fn request(
                &self,
                _session_id: &str,
                _tool: &str,
                _args: &Value,
                _tier: RiskTier,
                _preview: String,
                _cancel: &CancelToken,
            ) -> SessionApprovalDecision {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                SessionApprovalDecision::AllowAlways
            }
        }
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let proxy = DelegateProxy::for_agent(
            Arc::new(CountingApprover {
                calls: Arc::clone(&calls),
            }),
            "sess-codex".to_string(),
            DelegateDecisions::default(),
            "codex",
        );
        let cancel = CancelToken::never();
        let first = proxy.resolve_codex(
            &codex_request(1, "item/commandExecution/requestApproval", json!({})),
            &cancel,
        );
        let second = proxy.resolve_codex(
            &codex_request(2, "item/commandExecution/requestApproval", json!({})),
            &cancel,
        );
        // First → acceptForSession; the remembered allow serves the second as a
        // plain accept WITHOUT a second prompt.
        assert_eq!(first.decision, "acceptForSession");
        assert_eq!(second.decision, "accept");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn codex_tool_tier_classifies_and_fails_safe() {
        assert_eq!(codex_tool_tier("Edit"), RiskTier::Edit);
        assert_eq!(codex_tool_tier("Write"), RiskTier::Edit);
        for tool in ["Bash", "permissions", "anything"] {
            assert_eq!(codex_tool_tier(tool), RiskTier::Exec, "{tool}");
        }
    }
}
