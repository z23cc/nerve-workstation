//! Command→executor partition — the §10 routing table.
//!
//! The single place the command→executor partition is defined: [`Executor`] names
//! every owning executor and [`executor_for`] maps each [`RuntimeCommand`] to exactly
//! one. The parent's `dispatch_catching` routes through these; the totality test below
//! closes the architecture north star §10 hard gate at run time. Kept in its own file
//! so the parent [`super::JobManager`] lifecycle stays comfortably under the file-size
//! cap.

use nerve_runtime::RuntimeCommand;

/// The single executor that owns a [`RuntimeCommand`]. The `run_job` dispatch and
/// the §10 totality test both route through [`executor_for`], so this enum is the
/// one place the command→executor partition is defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Executor {
    /// The composition-root `agent.run` job (LLM orchestration).
    AgentRun,
    /// The host delegate runtime (`delegate.*` family): drives an external agent
    /// CLI subprocess. DA-1 ships a stub; DA-2 wires the real subprocess.
    Delegate,
    /// The host L0 run store (`run.*` family): enumerate/fetch captured Runs from
    /// the persisted [`RunStore`](crate::run_store) (read-only).
    Run,
    /// L0c deterministic replay (`replay.start`): re-drive a captured Run's tape and
    /// verify its content address against the recording.
    Replay,
    /// L1 evidence ledger (`ledger.query`): query the append-only cross-run log.
    Ledger,
    /// L2 execution-grounded verify (`verify.*`): re-run the org's checks in the
    /// pinned closure and seal/fetch the borrowed verdict.
    Verify,
    /// L3 policy plane (`policy.*`): serve the sealed policy doc + decision evidence.
    Policy,
    /// L4 receipt store (`receipt.get`): fetch a signed Verification Receipt.
    Receipt,
    /// L6 outcome corpus (`outcome.*`): append/get/query human/CI outcome labels.
    Outcome,
    /// The runtime host/native shell side-effects (`host.*` / `workspace.*`).
    Host,
    /// The host `SessionManager` (`session.*` family).
    Session,
    /// The host `AuthManager` (`auth.*` family).
    Auth,
    /// The host flow engine (`flow.*` family, C2): runs the deterministic C1
    /// orchestration engine as a job + the live-flow registry + approval routing.
    Flow,
    /// The daemon-hosted WeChat bridge (`wechat.*` family): QR login + the long-poll
    /// bridge that drives delegated turns in-process.
    Wechat,
    /// The core `Runtime` hub — nerve-core dispatch (`ping` / `tool.*`).
    CoreHub,
}

/// Map every protocol command to its single owning executor.
///
/// This is an **exhaustive** match on [`RuntimeCommand`] on purpose: it is the §10
/// hard gate. Adding a new variant breaks this match at COMPILE time, forcing an
/// explicit executor decision rather than letting the command fall through to the
/// core hub by default. Do not add a wildcard arm.
pub(super) fn executor_for(command: &RuntimeCommand) -> Executor {
    match command {
        RuntimeCommand::AgentRun { .. } => Executor::AgentRun,
        RuntimeCommand::DelegateStart { .. }
        | RuntimeCommand::DelegateSteer { .. }
        | RuntimeCommand::DelegateClose { .. }
        | RuntimeCommand::DelegateGet { .. }
        | RuntimeCommand::DelegateList => Executor::Delegate,
        RuntimeCommand::RunList
        | RuntimeCommand::RunGet { .. }
        | RuntimeCommand::OtelIngest { .. } => Executor::Run,
        RuntimeCommand::ReplayStart { .. } => Executor::Replay,
        RuntimeCommand::LedgerQuery { .. } | RuntimeCommand::LedgerVerify => Executor::Ledger,
        RuntimeCommand::VerifyStart { .. }
        | RuntimeCommand::VerifyGet { .. }
        | RuntimeCommand::VerifyList { .. } => Executor::Verify,
        RuntimeCommand::PolicyGet | RuntimeCommand::PolicyDecisions { .. } => Executor::Policy,
        RuntimeCommand::ReceiptGet { .. } => Executor::Receipt,
        RuntimeCommand::OutcomeLabel { .. }
        | RuntimeCommand::OutcomeGet { .. }
        | RuntimeCommand::OutcomeQuery { .. } => Executor::Outcome,
        RuntimeCommand::HostCapabilities
        | RuntimeCommand::HostClipboardWriteText { .. }
        | RuntimeCommand::HostNotificationShow { .. }
        | RuntimeCommand::HostFolderPick { .. }
        | RuntimeCommand::HostFileSaveText { .. }
        | RuntimeCommand::HostUrlOpen { .. }
        | RuntimeCommand::WorkspaceReveal { .. } => Executor::Host,
        RuntimeCommand::SessionStart { .. }
        | RuntimeCommand::SessionMessage { .. }
        | RuntimeCommand::SessionInterrupt { .. }
        | RuntimeCommand::SessionRespond { .. }
        | RuntimeCommand::SessionGet { .. }
        | RuntimeCommand::SessionList
        | RuntimeCommand::SessionClose { .. }
        | RuntimeCommand::SessionSetModel { .. }
        | RuntimeCommand::SessionSetMode { .. } => Executor::Session,
        RuntimeCommand::AuthStart { .. }
        | RuntimeCommand::AuthComplete { .. }
        | RuntimeCommand::AuthStatus { .. }
        | RuntimeCommand::AuthLease { .. }
        | RuntimeCommand::AuthLogout { .. } => Executor::Auth,
        RuntimeCommand::FlowStart { .. }
        | RuntimeCommand::FlowSteer { .. }
        | RuntimeCommand::FlowReplay { .. }
        | RuntimeCommand::FlowGet { .. }
        | RuntimeCommand::FlowList
        | RuntimeCommand::FlowClose { .. }
        | RuntimeCommand::FlowRespond { .. } => Executor::Flow,
        RuntimeCommand::WechatLogin { .. }
        | RuntimeCommand::WechatStart { .. }
        | RuntimeCommand::WechatStop
        | RuntimeCommand::WechatStatus => Executor::Wechat,
        RuntimeCommand::Ping | RuntimeCommand::ToolList | RuntimeCommand::ToolCall { .. } => {
            Executor::CoreHub
        }
    }
}

#[cfg(test)]
mod command_executor_partition {
    //! Governance test (architecture north star §10): the command-executor
    //! *totality* property, now backed by a **compile-time** hard gate.
    //! [`executor_for`] is an exhaustive match on [`RuntimeCommand`], so a new
    //! variant cannot compile until it is mapped to one [`Executor`] — there is no
    //! `else`/wildcard for it to fall through. These tests close the loop at
    //! run time: every name in the authoritative `RUNTIME_COMMAND_NAMES` maps to
    //! exactly one executor, so the wire vocabulary and the dispatch stay aligned
    //! (e.g. a `kind` rename that drifts from the constant is caught).
    use super::*;
    use serde_json::{Value, json};
    use std::collections::{BTreeSet, HashMap};

    /// Build a minimal representative value for a protocol command name by the
    /// real `kind`-tagged deserialization path (so a `kind` rename that drifts
    /// from `RUNTIME_COMMAND_NAMES` is caught too). Panics on an unknown name, so
    /// adding a name without teaching this test fails loudly — which then forces
    /// an executor decision in `every_runtime_command_has_exactly_one_executor`.
    fn representative(name: &str) -> RuntimeCommand {
        let fields: Value = match name {
            "ping" | "tool.list" | "session.list" | "flow.list" | "delegate.list" | "run.list"
            | "host.capabilities" | "workspace.reveal" => json!({}),
            "run.get" => json!({ "run_id": "r" }),
            "host.clipboard.write_text" => json!({ "text": "copy me" }),
            "host.notification.show" => json!({ "title": "Nerve", "body": "Done" }),
            "host.folder.pick" => json!({ "title": "Choose project folder" }),
            "host.file.save_text" => json!({
                "title": "Save packet",
                "default_name": "packet.md",
                "text": "# Packet"
            }),
            "host.url.open" => json!({ "url": "https://example.com/auth" }),
            "tool.call" => json!({ "name": "file_search" }),
            "agent.run" => json!({ "provider": "p", "model": "m", "task": "t" }),
            "session.start" => json!({ "provider": "p", "model": "m" }),
            "session.message" => json!({ "session_id": "s", "text": "t" }),
            "session.interrupt" | "session.get" | "session.close" => json!({ "session_id": "s" }),
            "session.respond" => {
                json!({ "session_id": "s", "request_id": "r", "decision": "allow" })
            }
            "session.set_model" => json!({ "session_id": "s", "model": "m" }),
            "session.set_mode" => json!({ "session_id": "s", "mode": "yolo" }),
            "auth.start" => json!({ "provider": "p", "flow": "browser" }),
            "auth.status" | "auth.logout" => json!({ "provider": "p" }),
            "auth.lease" => {
                json!({ "provider": "p", "force_refresh": false, "include_token": false })
            }
            "auth.complete" => json!({ "login_id": "l" }),
            "delegate.start" => json!({ "agent": "codex", "task": "t" }),
            "delegate.steer" => json!({ "session_id": "s", "message": "m" }),
            "delegate.close" | "delegate.get" => json!({ "session_id": "s" }),
            "flow.start" => json!({
                "workflow": {
                    "schema_version": 1,
                    "name": "n",
                    "strategy": {
                        "type": "single",
                        "step": { "worker": { "kind": "cli", "name": "claude" }, "task": "t" }
                    }
                }
            }),
            "flow.steer" => json!({ "flow_id": "f", "message": "m" }),
            "flow.replay" => json!({ "flow_id": "f" }),
            "flow.get" | "flow.close" => json!({ "flow_id": "f" }),
            "flow.respond" => json!({ "flow_id": "f", "request_id": "r", "decision": "allow" }),
            "wechat.login" | "wechat.start" | "wechat.stop" | "wechat.status" => json!({}),
            "replay.start" => json!({ "run_id": "r" }),
            "ledger.query" | "ledger.verify" | "policy.get" | "policy.decisions"
            | "verify.list" => json!({}),
            "verify.start" => json!({ "run_id": "r" }),
            "verify.get" => json!({ "verdict_id": "v" }),
            "receipt.get" => json!({ "receipt_id": "r" }),
            "otel.ingest" => json!({ "trace": {} }),
            "outcome.label" => json!({
                "run_id": "r",
                "outcome": { "outcome": "merged" },
                "source": { "source": "human" }
            }),
            "outcome.get" => json!({ "run_id": "r" }),
            "outcome.query" => json!({}),
            other => panic!(
                "RUNTIME_COMMAND_NAMES gained `{other}` with no representative here; add one and \
                 wire the variant to exactly one executor in `run_job`"
            ),
        };
        let mut object = fields.as_object().cloned().unwrap_or_default();
        object.insert("kind".to_string(), Value::String(name.to_string()));
        serde_json::from_value(Value::Object(object))
            .unwrap_or_else(|err| panic!("representative `{name}` failed to deserialize: {err}"))
    }

    #[test]
    fn every_runtime_command_maps_to_one_executor() {
        // `executor_for` is exhaustive (total) by construction — it would not
        // compile otherwise. This asserts the *name* table agrees: each protocol
        // name builds a command whose kind round-trips and resolves to exactly one
        // executor. A name added without a representative panics in `representative`
        // (and a kind drift is caught by the `name()` equality below).
        let mut seen_per_executor: HashMap<Executor, Vec<&str>> = HashMap::new();
        for &name in nerve_runtime::RUNTIME_COMMAND_NAMES {
            let command = representative(name);
            assert_eq!(
                command.name(),
                name,
                "representative for `{name}` built the wrong command kind"
            );
            // The match is exhaustive, so `executor_for` always returns exactly one
            // executor — no command can be unclaimed or double-claimed.
            let executor = executor_for(&command);
            seen_per_executor.entry(executor).or_default().push(name);
        }
        // Every executor must own at least one command (none is dead), and the
        // union of owned names must cover the whole vocabulary exactly once.
        let total: usize = seen_per_executor.values().map(Vec::len).sum();
        assert_eq!(
            total,
            nerve_runtime::RUNTIME_COMMAND_NAMES.len(),
            "executor map did not cover every command exactly once: {seen_per_executor:?}"
        );
        for executor in [
            Executor::AgentRun,
            Executor::Delegate,
            Executor::Run,
            Executor::Replay,
            Executor::Ledger,
            Executor::Verify,
            Executor::Policy,
            Executor::Receipt,
            Executor::Outcome,
            Executor::Host,
            Executor::Session,
            Executor::Auth,
            Executor::Flow,
            Executor::Wechat,
            Executor::CoreHub,
        ] {
            assert!(
                seen_per_executor.contains_key(&executor),
                "executor {executor:?} owns no command — dead executor arm in `run_job`"
            );
        }
    }

    #[test]
    fn executor_for_routes_each_family_to_its_owner() {
        // Spot-check the routing is by *family*, not incidental, so a misfiled
        // variant (e.g. an auth command routed to the session manager) is caught.
        assert_eq!(executor_for(&representative("ping")), Executor::CoreHub);
        assert_eq!(
            executor_for(&representative("tool.call")),
            Executor::CoreHub
        );
        assert_eq!(
            executor_for(&representative("agent.run")),
            Executor::AgentRun
        );
        assert_eq!(
            executor_for(&representative("session.start")),
            Executor::Session
        );
        assert_eq!(executor_for(&representative("auth.start")), Executor::Auth);
        for name in [
            "host.capabilities",
            "host.clipboard.write_text",
            "host.notification.show",
            "host.folder.pick",
            "host.file.save_text",
            "host.url.open",
            "workspace.reveal",
        ] {
            assert_eq!(
                executor_for(&representative(name)),
                Executor::Host,
                "`{name}` must route to the host executor"
            );
        }
        for name in [
            "flow.start",
            "flow.steer",
            "flow.replay",
            "flow.get",
            "flow.list",
            "flow.close",
            "flow.respond",
        ] {
            assert_eq!(
                executor_for(&representative(name)),
                Executor::Flow,
                "`{name}` must route to the flow executor"
            );
        }
    }

    #[test]
    fn runtime_command_names_have_no_duplicates() {
        let unique: BTreeSet<_> = nerve_runtime::RUNTIME_COMMAND_NAMES.iter().collect();
        assert_eq!(
            unique.len(),
            nerve_runtime::RUNTIME_COMMAND_NAMES.len(),
            "RUNTIME_COMMAND_NAMES contains duplicate entries"
        );
    }
}
