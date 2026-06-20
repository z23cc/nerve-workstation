//! DA-5c: the codex app-server wire protocol — message classification, the
//! per-turn accumulator, and the request/reply builders.
//!
//! The driver in [`super`] owns the [`PersistentChild`](crate::sandbox::PersistentChild)
//! and the turn loop; this submodule is the pure, table-like half: how an inbound
//! line is classified ([`classify`]), how a turn's streamed deltas fold into a
//! [`TurnResult`] ([`TurnAccumulator`]), and how each outbound frame is shaped
//! (argv, `thread/start` params, approval replies). Keeping it separate keeps the
//! driver focused on IO/cancellation and this on the protocol vocabulary.

use crate::delegate_runtime::DelegateUsage;
use crate::delegate_session::TurnResult;
use crate::sandbox::CommandSpec;
use nerve_runtime::DelegateAutonomy;
use serde_json::{Value, json};
use std::path::Path;

/// The `tool_output_token_limit` passed to codex (caps a single tool's captured
/// output the model sees; large enough for normal builds/tests).
const TOOL_OUTPUT_TOKEN_LIMIT: u64 = 32_000;

/// What one parsed line means for the in-flight turn loop.
pub(super) enum LineOutcome {
    Continue,
    Done,
    Interrupted,
}

/// The classification of one inbound app-server line (the dispatcher core).
#[derive(Debug, PartialEq)]
pub(super) enum Inbound {
    /// A response to one of our requests (`id` + `result`/`error`, no `method`).
    Response { id: i64 },
    /// A server→client request (`id` + `method`): we must reply.
    ServerRequest { id: i64, method: String },
    /// A server notification (`method`, no `id`).
    Notification { method: String },
    /// Anything that fits none of the above.
    Unknown,
}

/// Classify one inbound JSON object by the presence of `id` and `method`:
/// id+method = server request; method only = notification; id only = response.
pub(super) fn classify(value: &Value) -> Inbound {
    let id = value.get("id").and_then(Value::as_i64);
    let method = value.get("method").and_then(Value::as_str);
    match (id, method) {
        (Some(id), Some(method)) => Inbound::ServerRequest {
            id,
            method: method.to_string(),
        },
        (Some(id), None) => Inbound::Response { id },
        (None, Some(method)) => Inbound::Notification {
            method: method.to_string(),
        },
        (None, None) => Inbound::Unknown,
    }
}

/// Per-turn accumulator: the streamed agent-message text, the turn id (for
/// interrupt), the success flag, and the usage parsed from `tokenUsage` updates.
#[derive(Default)]
pub(super) struct TurnAccumulator {
    message: String,
    pub(super) turn_id: String,
    ok: bool,
    usage: Option<DelegateUsage>,
}

impl TurnAccumulator {
    /// Capture the turn id from the `turn/start` response (`{turn:{id}}`).
    pub(super) fn capture_turn_id(&mut self, response: &Value) {
        if let Some(id) = response
            .get("result")
            .and_then(|r| r.get("turn"))
            .and_then(|t| t.get("id"))
            .and_then(Value::as_str)
        {
            self.turn_id = id.to_string();
        }
    }

    /// Ingest one notification: accumulate agent-message deltas (streamed as
    /// progress), record usage, and signal completion on `turn/completed`.
    pub(super) fn ingest_notification(
        &mut self,
        method: &str,
        value: &Value,
        on_progress: &mut dyn FnMut(&str),
    ) -> LineOutcome {
        let params = value.get("params");
        match method {
            "item/agentMessage/delta" => {
                if let Some(delta) = params.and_then(|p| p.get("delta")).and_then(Value::as_str) {
                    self.message.push_str(delta);
                    on_progress(delta);
                }
                LineOutcome::Continue
            }
            "thread/tokenUsage/updated" => {
                if let Some(usage) = params.and_then(|p| p.get("usage")) {
                    self.usage = Some(parse_codex_usage(usage));
                }
                LineOutcome::Continue
            }
            "turn/completed" => {
                self.ok = params
                    .and_then(|p| p.get("turn"))
                    .and_then(|t| t.get("status"))
                    .and_then(Value::as_str)
                    .is_none_or(|s| s != "failed" && s != "error");
                LineOutcome::Done
            }
            _ => LineOutcome::Continue,
        }
    }

    /// Build the [`TurnResult`] from the accumulated stream state.
    pub(super) fn finish(&self) -> TurnResult {
        TurnResult {
            ok: self.ok,
            result: self.message.clone(),
            usage: self.usage,
            cost_usd: None,
        }
    }
}

/// Build the `codex app-server` argv. The two `-c` overrides set the per-tool
/// output cap and enable mid-turn steering; `app-server` selects the ndjson server.
///
/// DA-6: `mcp_disable_flags` are the pre-computed, sorted `-c
/// mcp_servers.<name>.enabled=false` pairs for the servers this delegated session
/// must skip (see [`crate::delegate_codex_mcp`]). They are inserted before
/// `app-server` so codex applies them at boot; an empty slice leaves the argv
/// unchanged (every configured MCP server boots, the pre-DA-6 behavior).
pub(super) fn build_codex_app_server_command(mcp_disable_flags: &[String]) -> CommandSpec {
    let mut args = vec![
        "-c".to_string(),
        format!("tool_output_token_limit={TOOL_OUTPUT_TOKEN_LIMIT}"),
        "-c".to_string(),
        "features.steer=true".to_string(),
    ];
    args.extend(mcp_disable_flags.iter().cloned());
    args.push("app-server".to_string());
    CommandSpec {
        command: "codex".to_string(),
        args,
    }
}

/// Build the `thread/start` params. Proxied mode asks Nerve for everything
/// (`approvalPolicy:untrusted` + `approvalsReviewer:user` + a restrictive
/// `read-only` sandbox); autonomy mode maps autonomy → sandbox with
/// `approvalPolicy:never` (codex governs itself, never asking).
pub(super) fn thread_start_params(
    cwd: &Path,
    autonomy: DelegateAutonomy,
    model: Option<&str>,
    proxied: bool,
) -> Value {
    let (approval_policy, sandbox) = if proxied {
        ("untrusted", "read-only")
    } else {
        ("never", autonomy_sandbox(autonomy))
    };
    let mut params = json!({
        "cwd": cwd.display().to_string(),
        "approvalPolicy": approval_policy,
        "sandbox": sandbox,
    });
    if proxied {
        params["approvalsReviewer"] = json!("user");
    }
    if let Some(model) = model {
        params["model"] = json!(model);
    }
    params
}

/// Map autonomy → codex sandbox (the autonomy-mode equivalent of the DA-2 one-shot
/// `--sandbox` recipe).
fn autonomy_sandbox(autonomy: DelegateAutonomy) -> &'static str {
    match autonomy {
        DelegateAutonomy::ReadOnly => "read-only",
        DelegateAutonomy::Edit => "workspace-write",
        DelegateAutonomy::Full => "danger-full-access",
    }
}

/// The `result` body of an approval reply. The decision rides under the shape the
/// approval method expects: command/file-change use `{decision}`; permission asks
/// use a permissions/scope object; a generic ask still carries `{decision}`.
///
/// A permission ask MUST honor the operator's decision: only an accept variant
/// (`accept` / `acceptForSession`) emits the session-scoped grant; a `decline` /
/// `cancel` replies with a non-grant body that never carries `scope:"session"`, so
/// an operator Deny can't be silently turned into a session-wide grant.
pub(super) fn approval_result(method: &str, decision: &str) -> Value {
    match method {
        "item/permissions/requestApproval" => {
            if is_accept_decision(decision) {
                json!({
                    "permissions": {},
                    "scope": "session",
                    "strictAutoReview": false,
                })
            } else {
                // A denied permission ask: no grant, and crucially no
                // `scope:"session"` — the absence of a session grant is the deny.
                json!({
                    "permissions": {},
                    "strictAutoReview": false,
                })
            }
        }
        _ => json!({ "decision": decision }),
    }
}

/// Whether a mapped codex decision is an accept variant. Only these grant the
/// session-scoped permission reply; `decline` / `cancel` (and anything else) do not.
fn is_accept_decision(decision: &str) -> bool {
    matches!(decision, "accept" | "acceptForSession")
}

/// A minimal safe reply for an unrecognised server-request (e.g.
/// `item/tool/requestUserInput`): decline / empty so the server doesn't block.
pub(super) fn minimal_server_reply(method: &str) -> Value {
    match method {
        m if m.ends_with("/requestUserInput") => json!({ "input": "" }),
        _ => json!({ "decision": "decline" }),
    }
}

/// Parse a codex `tokenUsage` object into [`DelegateUsage`]. codex reports
/// input/output and a cached-input figure; there is no cache-creation figure.
fn parse_codex_usage(usage: &Value) -> DelegateUsage {
    let get = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    DelegateUsage {
        input_tokens: get("input_tokens"),
        output_tokens: get("output_tokens"),
        cache_read_tokens: get("cached_input_tokens"),
        cache_creation_tokens: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_server_argv_sets_overrides_and_app_server() {
        // No disable flags (empty slice) -> the pre-DA-6 argv, unchanged.
        let spec = build_codex_app_server_command(&[]);
        assert_eq!(spec.command, "codex");
        assert_eq!(
            spec.args,
            vec![
                "-c",
                "tool_output_token_limit=32000",
                "-c",
                "features.steer=true",
                "app-server",
            ]
        );
    }

    #[test]
    fn app_server_argv_inserts_mcp_disable_flags_before_app_server() {
        // DA-6: disabled {b, c} -> their `-c …=false` pairs land between the
        // steer override and the `app-server` subcommand, in the order given
        // (callers pass an already-sorted set).
        let flags = crate::delegate_codex_mcp::disable_flags(&["b".to_string(), "c".to_string()]);
        let spec = build_codex_app_server_command(&flags);
        assert_eq!(
            spec.args,
            vec![
                "-c",
                "tool_output_token_limit=32000",
                "-c",
                "features.steer=true",
                "-c",
                "mcp_servers.b.enabled=false",
                "-c",
                "mcp_servers.c.enabled=false",
                "app-server",
            ]
        );
        // The allowed server "a" is never disabled.
        assert!(
            !spec.args.iter().any(|a| a == "mcp_servers.a.enabled=false"),
            "{:?}",
            spec.args
        );
    }

    #[test]
    fn classify_distinguishes_response_request_notification() {
        assert_eq!(
            classify(&json!({ "id": 3, "result": {} })),
            Inbound::Response { id: 3 }
        );
        assert_eq!(
            classify(
                &json!({ "id": 5, "method": "item/commandExecution/requestApproval", "params": {} })
            ),
            Inbound::ServerRequest {
                id: 5,
                method: "item/commandExecution/requestApproval".to_string()
            }
        );
        assert_eq!(
            classify(&json!({ "method": "item/agentMessage/delta", "params": {} })),
            Inbound::Notification {
                method: "item/agentMessage/delta".to_string()
            }
        );
        assert_eq!(classify(&json!({ "foo": 1 })), Inbound::Unknown);
        // An error response is still a response (routed by id).
        assert_eq!(
            classify(&json!({ "id": 9, "error": { "code": -1 } })),
            Inbound::Response { id: 9 }
        );
    }

    #[test]
    fn proxied_thread_start_asks_nerve_for_everything() {
        let params = thread_start_params(Path::new("/w"), DelegateAutonomy::Full, None, true);
        assert_eq!(params["approvalPolicy"], "untrusted");
        assert_eq!(params["approvalsReviewer"], "user");
        // Proxied mode pins a restrictive sandbox regardless of autonomy.
        assert_eq!(params["sandbox"], "read-only");
        assert_eq!(params["cwd"], "/w");
    }

    #[test]
    fn autonomy_thread_start_maps_sandbox_and_never_asks() {
        let ro = thread_start_params(Path::new("/w"), DelegateAutonomy::ReadOnly, None, false);
        assert_eq!(ro["approvalPolicy"], "never");
        assert_eq!(ro["sandbox"], "read-only");
        assert!(ro.get("approvalsReviewer").is_none());

        let edit = thread_start_params(Path::new("/w"), DelegateAutonomy::Edit, None, false);
        assert_eq!(edit["sandbox"], "workspace-write");

        let full = thread_start_params(Path::new("/w"), DelegateAutonomy::Full, Some("o3"), false);
        assert_eq!(full["sandbox"], "danger-full-access");
        assert_eq!(full["model"], "o3");
    }

    #[test]
    fn accumulator_streams_deltas_and_flushes_on_completed() {
        let mut acc = TurnAccumulator::default();
        acc.capture_turn_id(&json!({ "id": 3, "result": { "turn": { "id": "turn-1" } } }));
        assert_eq!(acc.turn_id, "turn-1");

        let mut streamed = Vec::new();
        let mut on_progress = |t: &str| streamed.push(t.to_string());

        let cont = acc.ingest_notification(
            "item/agentMessage/delta",
            &json!({ "params": { "delta": "Hello " } }),
            &mut on_progress,
        );
        assert!(matches!(cont, LineOutcome::Continue));
        acc.ingest_notification(
            "item/agentMessage/delta",
            &json!({ "params": { "delta": "world" } }),
            &mut on_progress,
        );
        acc.ingest_notification(
            "thread/tokenUsage/updated",
            &json!({ "params": { "usage": { "input_tokens": 10, "output_tokens": 4, "cached_input_tokens": 2 } } }),
            &mut on_progress,
        );
        let done = acc.ingest_notification(
            "turn/completed",
            &json!({ "params": { "turn": { "id": "turn-1", "status": "completed" } } }),
            &mut on_progress,
        );
        assert!(matches!(done, LineOutcome::Done));

        assert_eq!(streamed, vec!["Hello ", "world"]);
        let result = acc.finish();
        assert!(result.ok);
        assert_eq!(result.result, "Hello world");
        assert_eq!(
            result.usage,
            Some(DelegateUsage {
                input_tokens: 10,
                output_tokens: 4,
                cache_read_tokens: 2,
                cache_creation_tokens: 0,
            })
        );
        assert_eq!(result.cost_usd, None);
    }

    #[test]
    fn failed_turn_status_is_not_ok() {
        let mut acc = TurnAccumulator::default();
        let mut sink = |_: &str| {};
        acc.ingest_notification(
            "turn/completed",
            &json!({ "params": { "turn": { "id": "t", "status": "failed" } } }),
            &mut sink,
        );
        assert!(!acc.finish().ok);
    }

    #[test]
    fn approval_result_shapes_per_method() {
        assert_eq!(
            approval_result("item/commandExecution/requestApproval", "accept"),
            json!({ "decision": "accept" })
        );
        assert_eq!(
            approval_result("item/fileChange/requestApproval", "decline"),
            json!({ "decision": "decline" })
        );
        // A DENIED permissions ask must NOT carry the session grant: no
        // `scope:"session"` (its absence is the deny). A Deny that became a
        // session-wide grant was the approval-bypass bug.
        let perm_deny = approval_result("item/permissions/requestApproval", "decline");
        assert!(
            perm_deny.get("scope").is_none(),
            "a denied permission reply must not carry a scope: {perm_deny}"
        );
        assert_eq!(perm_deny["strictAutoReview"], false);
        assert!(perm_deny["permissions"].is_object());
        // A `cancel` deny is likewise non-granting.
        let perm_cancel = approval_result("item/permissions/requestApproval", "cancel");
        assert!(perm_cancel.get("scope").is_none(), "{perm_cancel}");

        // An ACCEPTED permissions ask DOES carry the session-scoped grant.
        for accept in ["accept", "acceptForSession"] {
            let perm = approval_result("item/permissions/requestApproval", accept);
            assert_eq!(perm["scope"], "session", "{accept}");
            assert_eq!(perm["strictAutoReview"], false);
            assert!(perm["permissions"].is_object());
        }
    }

    #[test]
    fn minimal_server_reply_handles_user_input_and_falls_back() {
        assert_eq!(
            minimal_server_reply("item/tool/requestUserInput"),
            json!({ "input": "" })
        );
        assert_eq!(
            minimal_server_reply("something/else"),
            json!({ "decision": "decline" })
        );
    }
}
