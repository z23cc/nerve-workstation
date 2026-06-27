use crate::outcome::{LabelSource, Outcome};
use crate::verdict::{CheckKind, VerdictStatus};
#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

mod aux;
mod delegate;
mod runtime_command_impl;

pub use aux::{
    ApprovalMode, FlowSource, LedgerRef, OtelSource, SessionApprovalDecision, WorkerSelector,
};
pub use delegate::{DelegateAutonomy, DelegateRole};

/// Runtime command kinds accepted by the human-facing daemon job protocol.
pub const RUNTIME_COMMAND_NAMES: &[&str] = &[
    "ping",
    "tool.list",
    "tool.call",
    "agent.run",
    "session.start",
    "session.message",
    "session.interrupt",
    "session.respond",
    "session.get",
    "session.list",
    "session.close",
    "session.set_model",
    "session.set_mode",
    "auth.start",
    "auth.complete",
    "auth.status",
    "auth.lease",
    "auth.logout",
    "delegate.start",
    "delegate.steer",
    "delegate.close",
    "delegate.get",
    "delegate.list",
    "run.list",
    "run.get",
    "replay.start",
    "ledger.query",
    "ledger.verify",
    "verify.start",
    "verify.get",
    "verify.list",
    "policy.get",
    "policy.decisions",
    "receipt.get",
    "otel.ingest",
    "outcome.label",
    "outcome.get",
    "outcome.query",
    "flow.start",
    "flow.steer",
    "flow.replay",
    "flow.get",
    "flow.list",
    "flow.close",
    "flow.respond",
    "host.capabilities",
    "host.clipboard.write_text",
    "host.notification.show",
    "host.folder.pick",
    "host.file.save_text",
    "host.url.open",
    "workspace.reveal",
    "wechat.login",
    "wechat.start",
    "wechat.stop",
    "wechat.status",
];

/// Login flow requested by `auth.start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AuthStartFlow {
    /// Browser authorization-code + PKCE flow over loopback/manual paste.
    #[default]
    Browser,
    /// OAuth 2.0 device authorization flow for remote/mobile clients.
    DeviceCode,
}

/// Transport-neutral command understood by human-facing runtime adapters.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(tag = "kind")]
pub enum RuntimeCommand {
    /// Lightweight health check used by clients before opening a real session.
    #[serde(rename = "ping")]
    Ping,
    /// Return all runtime tool specifications.
    #[serde(rename = "tool.list")]
    ToolList,
    /// Execute one MCP-style tool through the runtime dispatcher.
    #[serde(rename = "tool.call")]
    ToolCall {
        name: String,
        #[serde(default = "default_arguments")]
        arguments: BTreeMap<String, Value>,
    },
    /// Run the built-in agent loop as a job. This is protocol vocabulary only:
    /// the host job manager (the composition root) executes it; the core runtime
    /// dispatcher does not (it has no LLM/provider knowledge). Provider/model are
    /// plain data here and translated to domain types by the host.
    #[serde(rename = "agent.run")]
    AgentRun {
        provider: String,
        model: String,
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_turns: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        temperature: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_filter: Option<Vec<String>>,
    },
    /// Start or resume a host-managed interactive agent session.
    #[serde(rename = "session.start")]
    SessionStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        provider: String,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_turns: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        temperature: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_filter: Option<Vec<String>>,
    },
    /// Send a user message to an existing host-managed session.
    #[serde(rename = "session.message")]
    SessionMessage { session_id: String, text: String },
    /// Interrupt the current turn of an existing host-managed session.
    #[serde(rename = "session.interrupt")]
    SessionInterrupt { session_id: String },
    /// Reply to a session approval request.
    #[serde(rename = "session.respond")]
    SessionRespond {
        session_id: String,
        request_id: String,
        decision: SessionApprovalDecision,
    },
    /// Fetch one host-managed session.
    #[serde(rename = "session.get")]
    SessionGet { session_id: String },
    /// List host-managed sessions.
    #[serde(rename = "session.list")]
    SessionList,
    /// Close a host-managed session.
    #[serde(rename = "session.close")]
    SessionClose { session_id: String },
    /// Switch the model (and optionally provider) of a live session in place,
    /// keeping its history and checkpoint. Takes effect from the next turn.
    #[serde(rename = "session.set_model")]
    SessionSetModel {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        model: String,
    },
    /// Switch the approval mode of a live session in place. Takes effect from the
    /// next gate decision. Pure protocol vocabulary; the host session manager
    /// stores it (P2 consults it in the gate).
    #[serde(rename = "session.set_mode")]
    SessionSetMode {
        session_id: String,
        mode: ApprovalMode,
    },
    /// Start a host-managed OAuth login.
    #[serde(rename = "auth.start")]
    AuthStart {
        provider: String,
        /// Requested login flow. Defaults to browser authorization-code flow.
        #[serde(default, skip_serializing_if = "AuthStartFlow::is_default")]
        flow: AuthStartFlow,
    },
    /// Complete a host-managed OAuth login with a code or pasted callback URL.
    #[serde(rename = "auth.complete")]
    AuthComplete {
        login_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        callback_url: Option<String>,
    },
    /// Return stored OAuth/API-key credential status without secrets.
    #[serde(rename = "auth.status")]
    AuthStatus { provider: String },
    /// Ask the trusted host to mint OAuth lease metadata and, for trusted native
    /// clients, optionally include a short-lived access token. The stored refresh
    /// token is never returned.
    #[serde(rename = "auth.lease")]
    AuthLease {
        provider: String,
        /// Force the broker to refresh before returning a lease.
        #[serde(default)]
        force_refresh: bool,
        /// Include the short-lived access token in the response. Defaults to true
        /// for backwards-compatible native clients; browser clients should pass false.
        #[serde(
            default = "default_auth_lease_include_token",
            skip_serializing_if = "is_true"
        )]
        include_token: bool,
    },
    /// Remove stored credentials for a provider.
    #[serde(rename = "auth.logout")]
    AuthLogout { provider: String },
    /// Delegate a coding task to an external agent CLI (codex / claude)
    /// as a long-lived job. Pure protocol vocabulary: the host job manager drives
    /// the subprocess (DA-2); `nerve-core` has no subprocess knowledge. `agent` is
    /// the catalog name from `list_agents`; `cwd` defaults to the workspace root;
    /// `model` overrides the agent's default model. Progress streams back as
    /// [`crate::RuntimeEvent::DelegateProgress`].
    #[serde(rename = "delegate.start")]
    DelegateStart {
        agent: String,
        task: String,
        /// The workspace whose root confines this delegated run (`cwd` resolves under
        /// that root). Resolves the sole workspace when omitted, but is REQUIRED once
        /// more than one workspace is registered — otherwise resolution is ambiguous
        /// and the start fails. Mirrors `SessionStart`'s `workspace`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default)]
        autonomy: DelegateAutonomy,
        /// Behavior preset for the delegated agent (DA-7). Defaults to
        /// [`DelegateRole::Standard`] (passthrough); [`DelegateRole::Scout`] makes
        /// it a read-only repository explorer that returns compact citations and
        /// forces read-only autonomy regardless of the `autonomy` field.
        #[serde(default, skip_serializing_if = "DelegateRole::is_default")]
        role: DelegateRole,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// DA-6 (codex only): the MCP allowlist for this delegated codex session —
        /// the `[mcp_servers.<name>]` entries to keep enabled; every other
        /// configured server is disabled for a fast start. `Some(list)` overrides
        /// the persisted `[delegate.codex] mcp_enable` config (an empty list
        /// disables ALL); `None` falls back to that config. Ignored for non-codex
        /// agents.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mcp_enable: Option<Vec<String>>,
    },
    /// Steer a live delegated session with a follow-up user message, running one
    /// more turn against the same long-lived agent process. Pure protocol
    /// vocabulary: the host job manager looks up the live session (DA-5a) and
    /// continues it; progress streams back as [`crate::RuntimeEvent::DelegateProgress`].
    /// `session_id` is the `job_id` of the originating [`Self::DelegateStart`] job
    /// (a started delegated session keeps that id for its whole lifetime).
    #[serde(rename = "delegate.steer")]
    DelegateSteer { session_id: String, message: String },
    /// End a live delegated session: close the agent process's stdin (which it
    /// treats as EOF and exits on) and reap it. Pure protocol vocabulary; the host
    /// job manager deregisters the live session. `session_id` is the originating
    /// [`Self::DelegateStart`] job id (see [`Self::DelegateSteer`]).
    #[serde(rename = "delegate.close")]
    DelegateClose { session_id: String },
    /// Fetch one live delegated session by id (the originating
    /// [`Self::DelegateStart`] job id). Read-only; mirrors `session.get` /
    /// `flow.get`. An unknown id is an error. The result is
    /// `{ "delegate": { session_id, agent, status, agent_session_id } }`.
    #[serde(rename = "delegate.get")]
    DelegateGet { session_id: String },
    /// List the live delegated sessions the host is parking, so a cockpit can
    /// observe its whole external-agent fleet over the protocol (not just from a
    /// single client's local state). Read-only; mirrors `session.list` /
    /// `flow.list`. The result is `{ "delegates": [ { session_id, agent, status,
    /// agent_session_id }, … ] }`, sorted by `session_id`.
    #[serde(rename = "delegate.list")]
    DelegateList,
    /// List captured Runs — the L0 flight-recorder index (`trust-substrate.md`
    /// §6). Read-only; mirrors `delegate.list` / `flow.list`. Pure protocol
    /// vocabulary: the host reads the persisted `RunStore`; `nerve-core` has no
    /// store. The result is `{ "runs": [ <Run>, … ] }`, newest `run_id` first.
    #[serde(rename = "run.list")]
    RunList,
    /// Fetch one captured Run by its content-addressed id (`trust-substrate.md`
    /// §6). Read-only; an unknown id is an error. The result is the full
    /// [`crate::provenance::Run`] (its event tape + content-addressed ledger), so
    /// a client can replay or re-verify it.
    #[serde(rename = "run.get")]
    RunGet { run_id: String },
    /// L0c — deterministically replay a captured Run: re-drive its recorded event
    /// tape (no model call) and assert the replayed spine head equals the recorded
    /// one. Runs as a job emitting `replay_progress` / `replay_finished`.
    #[serde(rename = "replay.start")]
    ReplayStart { run_id: String },
    /// L1 — query the append-only cross-run evidence ledger (read-only). Optional
    /// filters narrow by run / agent / diff / run-root-hash lineage / verdict outcome /
    /// record kind.
    #[serde(rename = "ledger.query")]
    LedgerQuery {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff_hash: Option<String>,
        /// Filter to the lineage of one run by its content address (`run_root_hash`):
        /// matches the `RunRecorded` whose hash this is, plus the post-wave-3
        /// `Verdict`/`ReceiptIssued` records that pin back to it (v13).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_root_hash: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<VerdictStatus>,
        // NOT `kind`: that field name collides with the enum's internal `tag = "kind"`
        // discriminant. Filter by ledger record kind under a distinct wire name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        record_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u64>,
    },
    /// L1 — re-derive the append-only evidence ledger and report whether the hash
    /// chain is intact (read-only; the tamper-detection moat). No facets: the whole
    /// chain is re-derived via `nerve_core::ledger::verify_chain`. The result is
    /// `{ "ok": true, "count": N, "head_hash": "…" }` on an intact chain, or
    /// `{ "ok": false, "error": "<HashMismatch|SeqGap|PrevMismatch>", "seq": K }`
    /// pointing at the first record where the re-derivation diverged.
    #[serde(rename = "ledger.verify")]
    LedgerVerify,
    /// L2 — re-run the org's own checks over a Run's diff in the pinned closure,
    /// producing an execution-grounded Verdict. The authority is BORROWED from the
    /// org's CI (INV-R3); Nerve never invents a correctness verdict.
    #[serde(rename = "verify.start")]
    VerifyStart {
        run_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reruns: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        only: Option<Vec<CheckKind>>,
    },
    /// L2 — fetch one sealed Verdict by id (read-only).
    #[serde(rename = "verify.get")]
    VerifyGet { verdict_id: String },
    /// L2 — list sealed Verdicts, optionally for one run (read-only).
    #[serde(rename = "verify.list")]
    VerifyList {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
    },
    /// L3 — return the sealed policy-as-code document in force (read-only).
    #[serde(rename = "policy.get")]
    PolicyGet,
    /// L3 — list recorded policy grant/denial decisions, optionally for one session.
    #[serde(rename = "policy.decisions")]
    PolicyDecisions {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// L4 — fetch a signed, portable Verification Receipt by id (read-only). The
    /// Receipt is third-party re-verifiable offline.
    #[serde(rename = "receipt.get")]
    ReceiptGet { receipt_id: String },
    /// L5 — ingest an external OTel-GenAI trace into a `Partial`-attestation Run, so
    /// even agents Nerve did not instrument are partially attested from their traces.
    #[serde(rename = "otel.ingest")]
    OtelIngest {
        #[serde(flatten)]
        source: OtelSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
    },
    /// L6 — append a human/CI outcome label (merged / reverted / incident /
    /// shipped-no-regress) to a run's outcome record (the cross-agent corpus).
    #[serde(rename = "outcome.label")]
    OutcomeLabel {
        run_id: String,
        outcome: Outcome,
        source: LabelSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verdict_ref: Option<String>,
    },
    /// L6 — fetch one run's outcome record (read-only).
    #[serde(rename = "outcome.get")]
    OutcomeGet { run_id: String },
    /// L6 — query the outcome corpus, optionally by agent / outcome (read-only).
    #[serde(rename = "outcome.query")]
    OutcomeQuery {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<Outcome>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u64>,
    },
    /// Start a declarative orchestration workflow (the Conductor, design §4) as one
    /// cancellable **job**: the host job manager runs the deterministic flow engine
    /// (C1) and the `job_id` IS the `flow_id`. The `workflow` is either an inline
    /// [`WorkflowDef`] or a named reference resolved from a loaded `WorkflowDef`
    /// data file ([`FlowSource`]). Progress streams back as the `flow_*` events
    /// ([`crate::RuntimeEvent::FlowStarted`] …); approvals reuse
    /// [`crate::RuntimeEvent::ApprovalRequested`] keyed by `flow_id`.
    #[serde(rename = "flow.start")]
    FlowStart {
        #[serde(flatten)]
        workflow: FlowSource,
        /// Named-output seeds the engine exposes to the first node's task template.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inputs: Option<BTreeMap<String, String>>,
        /// Workspace to run against when more than one is registered.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
    },
    /// Steer a live flow branch with a follow-up message, running one more turn
    /// against the same live worker session (design §4 / Wave C3a). Reuses the C0
    /// `WorkerSession::steer` port per-worker, exactly as `delegate.steer` does
    /// for a single delegated session: the host job manager looks up the live
    /// worker for `target` in the live-flow worker registry and continues it;
    /// progress streams back as [`crate::RuntimeEvent::FlowNodeAgent`]. Only a
    /// live, steerable branch (a `Single`/`Pipeline` worker still in flight, on a
    /// steerable substrate) can be steered; a closed or one-shot worker
    /// (a remote/MCP worker) returns a clear error. `flow_id` is the originating
    /// [`Self::FlowStart`] job id; `target` selects which branch (by node id, or
    /// the only live worker when unset).
    #[serde(rename = "flow.steer")]
    FlowSteer {
        flow_id: String,
        #[serde(default, skip_serializing_if = "WorkerSelector::is_default")]
        target: WorkerSelector,
        message: String,
    },
    /// Deterministically REPLAY a recorded flow offline (the audit verb, design §3/§4).
    /// The host loads the recorded `WorkerLedger` from the `FlowStore` by
    /// [`LedgerRef`], runs the SAME deterministic engine in REPLAY mode — a
    /// `ReplayWorker` re-emits the recorded `WorkerEvent`s/`TurnResult`s instead of
    /// calling any LLM/subprocess — and re-emits the `flow_*` event stream. Runs as
    /// one cancellable **job** (the `job_id` IS the replayed `flow_id`); the replay is
    /// byte-identical to the recorded run (the CI gate), at zero cost. `workspace`
    /// scopes which project's `.nerve/flows` the ledger is loaded from when more than
    /// one is registered.
    #[serde(rename = "flow.replay")]
    FlowReplay {
        #[serde(flatten)]
        ledger_ref: LedgerRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
    },
    /// Fetch one live or recently-finished flow by id. Mirrors `session.get`.
    #[serde(rename = "flow.get")]
    FlowGet { flow_id: String },
    /// List flows the host knows about. Mirrors `session.list`.
    #[serde(rename = "flow.list")]
    FlowList,
    /// Close (cancel) a live flow, tearing its workers down. Mirrors
    /// `session.close` / `delegate.close`. `flow_id` is the originating
    /// [`Self::FlowStart`] job id.
    #[serde(rename = "flow.close")]
    FlowClose { flow_id: String },
    /// Reply to a flow approval request. **Reuses** the existing
    /// [`SessionApprovalDecision`] + the host `ApprovalHub` round-trip, keyed by
    /// `flow_id` (a flow branch is just another approval id). No new approval type.
    #[serde(rename = "flow.respond")]
    FlowRespond {
        flow_id: String,
        request_id: String,
        decision: SessionApprovalDecision,
    },
    /// Return concrete host/native affordances reachable through the daemon.
    #[serde(rename = "host.capabilities")]
    HostCapabilities,
    /// Write plain text to the host OS clipboard through the daemon.
    #[serde(rename = "host.clipboard.write_text")]
    HostClipboardWriteText { text: String },
    /// Show a native OS notification through the host daemon.
    #[serde(rename = "host.notification.show")]
    HostNotificationShow {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
    },
    /// Open a native host folder picker and return the selected absolute path.
    #[serde(rename = "host.folder.pick")]
    HostFolderPick { title: Option<String> },
    /// Save UTF-8 text through a native host save panel.
    #[serde(rename = "host.file.save_text")]
    HostFileSaveText {
        title: Option<String>,
        default_name: Option<String>,
        text: String,
    },
    /// Open an external http(s) URL with the host OS default handler.
    #[serde(rename = "host.url.open")]
    HostUrlOpen { url: String },
    /// Reveal a served workspace root in the OS file manager through the daemon.
    #[serde(rename = "workspace.reveal")]
    WorkspaceReveal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
    },
    /// Begin a personal-WeChat (个人微信) QR login through the iLink Bot gateway as
    /// a long-lived **job**: the host fetches a QR, streams `wechat` login events
    /// ([`crate::WechatEventKind::LoginQr`] → `LoginStatus` → `LoggedIn`), and on
    /// success caches the session so [`Self::WechatStart`] can run the bridge. Pure
    /// protocol vocabulary; the daemon's WeChat host executes it (it is network +
    /// wall-clock, never `nerve-core`). The job is cancellable (cancel aborts the
    /// QR poll). `bot_type` is the iLink bot registration type; it defaults to
    /// `"3"` (`DEFAULT_ILINK_BOT_TYPE` — the value tools like Hermes Agent bake in),
    /// so login is **scan-only** and the field can be omitted.
    #[serde(rename = "wechat.login")]
    WechatLogin {
        #[serde(default = "runtime_command_impl::default_wechat_bot_type")]
        bot_type: String,
        /// Login bootstrap host; defaults to the iLink default when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
    },
    /// Start the WeChat→nerve bridge against the logged-in session: each allowed
    /// owner's inbound message drives one `delegate.*` turn (read-only by default)
    /// and the reply is sent back to the chat. Requires a prior successful
    /// [`Self::WechatLogin`] and a daemon started with `--allow-delegate`. Account
    /// safety is fail-closed: an empty `owners` list denies everyone. Progress and
    /// relayed messages stream back as [`crate::RuntimeEvent::Wechat`] events.
    #[serde(rename = "wechat.start")]
    WechatStart {
        /// WeChat user ids permitted to drive the agent (empty = deny all).
        #[serde(default)]
        owners: Vec<String>,
        /// Delegate agent catalog name (`claude` / `codex`).
        #[serde(default = "runtime_command_impl::default_wechat_agent")]
        agent: String,
        /// Autonomy granted to each delegated turn (defaults to read-only).
        #[serde(default)]
        autonomy: DelegateAutonomy,
    },
    /// Stop the running WeChat bridge (idempotent: a no-op when none is running).
    /// The logged-in session is retained so a later [`Self::WechatStart`] needs no
    /// re-scan.
    #[serde(rename = "wechat.stop")]
    WechatStop,
    /// Report WeChat login + bridge status (logged-in account/user, whether the
    /// bridge is running, the configured owners). Returns immediately.
    #[serde(rename = "wechat.status")]
    WechatStatus,
}

fn default_arguments() -> BTreeMap<String, Value> {
    BTreeMap::new()
}

fn default_auth_lease_include_token() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

impl AuthStartFlow {
    fn is_default(value: &Self) -> bool {
        *value == Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RiskTier;

    #[test]
    fn session_set_model_round_trips() {
        let value = serde_json::json!({
            "kind": "session.set_model",
            "session_id": "s1",
            "model": "grok-4-fast",
        });
        let command: RuntimeCommand = serde_json::from_value(value).expect("parse set_model");
        assert_eq!(command.name(), "session.set_model");
        assert_eq!(command.tool_name(), None);
        match command {
            RuntimeCommand::SessionSetModel {
                session_id,
                provider,
                model,
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(provider, None);
                assert_eq!(model, "grok-4-fast");
            }
            other => panic!("unexpected variant: {}", other.name()),
        }
        // session.set_model is listed in the canonical command-name set.
        assert!(RUNTIME_COMMAND_NAMES.contains(&"session.set_model"));
    }

    #[test]
    fn session_set_mode_round_trips() {
        let value = serde_json::json!({
            "kind": "session.set_mode",
            "session_id": "s1",
            "mode": "write",
        });
        let command: RuntimeCommand = serde_json::from_value(value).expect("parse set_mode");
        assert_eq!(command.name(), "session.set_mode");
        assert_eq!(command.tool_name(), None);
        match command {
            RuntimeCommand::SessionSetMode { session_id, mode } => {
                assert_eq!(session_id, "s1");
                assert_eq!(mode, ApprovalMode::Write);
            }
            other => panic!("unexpected variant: {}", other.name()),
        }
        assert!(RUNTIME_COMMAND_NAMES.contains(&"session.set_mode"));
    }

    #[test]
    fn auth_start_round_trips_with_default_browser_flow() {
        assert!(matches!(
            RuntimeCommand::auth_start("chatgpt"),
            RuntimeCommand::AuthStart {
                flow: AuthStartFlow::Browser,
                ..
            }
        ));
        assert!(matches!(
            RuntimeCommand::auth_start_with_flow("chatgpt", AuthStartFlow::DeviceCode),
            RuntimeCommand::AuthStart {
                flow: AuthStartFlow::DeviceCode,
                ..
            }
        ));

        let value = serde_json::json!({
            "kind": "auth.start",
            "provider": "chatgpt",
        });
        let command: RuntimeCommand = serde_json::from_value(value).expect("parse auth.start");
        assert_eq!(command.name(), "auth.start");
        match command {
            RuntimeCommand::AuthStart { provider, flow } => {
                assert_eq!(provider, "chatgpt");
                assert_eq!(flow, AuthStartFlow::Browser);
            }
            other => panic!("unexpected variant: {}", other.name()),
        }

        let value = serde_json::json!({
            "kind": "auth.start",
            "provider": "chatgpt",
            "flow": "device_code",
        });
        let command: RuntimeCommand =
            serde_json::from_value(value).expect("parse device auth.start");
        assert!(matches!(
            command,
            RuntimeCommand::AuthStart {
                flow: AuthStartFlow::DeviceCode,
                ..
            }
        ));
    }

    #[test]
    fn auth_lease_round_trips_with_default_force_refresh() {
        let value = serde_json::json!({
            "kind": "auth.lease",
            "provider": "chatgpt",
        });
        let command: RuntimeCommand = serde_json::from_value(value).expect("parse auth.lease");
        assert_eq!(command.name(), "auth.lease");
        assert_eq!(command.tool_name(), None);
        match command {
            RuntimeCommand::AuthLease {
                provider,
                force_refresh,
                include_token,
            } => {
                assert_eq!(provider, "chatgpt");
                assert!(!force_refresh);
                assert!(include_token);
            }
            other => panic!("unexpected variant: {}", other.name()),
        }
        assert!(RUNTIME_COMMAND_NAMES.contains(&"auth.lease"));

        let metadata_only = serde_json::json!({
            "kind": "auth.lease",
            "provider": "chatgpt",
            "include_token": false,
        });
        let command: RuntimeCommand =
            serde_json::from_value(metadata_only).expect("metadata lease");
        assert!(matches!(
            command,
            RuntimeCommand::AuthLease {
                include_token: false,
                ..
            }
        ));
    }

    #[test]
    fn delegate_start_round_trips_with_default_autonomy() {
        // `autonomy` and `model`/`cwd` omitted: autonomy defaults to the most
        // restricted tier, optionals to None.
        let value = serde_json::json!({
            "kind": "delegate.start",
            "agent": "codex",
            "task": "add a test",
        });
        let command: RuntimeCommand = serde_json::from_value(value).expect("parse delegate.start");
        assert_eq!(command.name(), "delegate.start");
        assert_eq!(command.tool_name(), None);
        match command {
            RuntimeCommand::DelegateStart {
                agent,
                task,
                workspace,
                cwd,
                autonomy,
                role,
                model,
                mcp_enable,
            } => {
                assert_eq!(agent, "codex");
                assert_eq!(task, "add a test");
                assert_eq!(workspace, None);
                assert_eq!(cwd, None);
                assert_eq!(autonomy, DelegateAutonomy::ReadOnly);
                assert_eq!(role, DelegateRole::Standard);
                assert_eq!(model, None);
                assert_eq!(mcp_enable, None);
            }
            other => panic!("unexpected variant: {}", other.name()),
        }
        assert!(RUNTIME_COMMAND_NAMES.contains(&"delegate.start"));
    }

    #[test]
    fn delegate_start_round_trips_mcp_enable_allowlist() {
        // DA-6: a per-call codex MCP allowlist round-trips (and an empty list is a
        // valid override meaning "disable all").
        let value = serde_json::json!({
            "kind": "delegate.start",
            "agent": "codex",
            "task": "investigate",
            "mcp_enable": ["chrome-devtools"],
        });
        let command: RuntimeCommand = serde_json::from_value(value).expect("parse with allowlist");
        match command {
            RuntimeCommand::DelegateStart { mcp_enable, .. } => {
                assert_eq!(mcp_enable, Some(vec!["chrome-devtools".to_string()]));
            }
            other => panic!("unexpected variant: {}", other.name()),
        }

        // Re-serialize: `mcp_enable` is present when Some, absent when None.
        let with = RuntimeCommand::DelegateStart {
            agent: "codex".into(),
            task: "t".into(),
            workspace: None,
            cwd: None,
            autonomy: DelegateAutonomy::ReadOnly,
            role: DelegateRole::Standard,
            model: None,
            mcp_enable: Some(vec![]),
        };
        let json = serde_json::to_value(&with).expect("serialize Some([])");
        assert_eq!(json["mcp_enable"], serde_json::json!([]));
        let without = RuntimeCommand::DelegateStart {
            agent: "codex".into(),
            task: "t".into(),
            workspace: None,
            cwd: None,
            autonomy: DelegateAutonomy::ReadOnly,
            role: DelegateRole::Standard,
            model: None,
            mcp_enable: None,
        };
        let json = serde_json::to_value(&without).expect("serialize None");
        assert!(json.get("mcp_enable").is_none(), "None is skipped: {json}");
        // The default role is kept off the wire (skip_serializing_if).
        assert!(
            json.get("role").is_none(),
            "default role is skipped: {json}"
        );
    }

    #[test]
    fn run_commands_round_trip_and_are_named() {
        let list: RuntimeCommand =
            serde_json::from_value(serde_json::json!({ "kind": "run.list" }))
                .expect("parse run.list");
        assert_eq!(list.name(), "run.list");
        assert_eq!(list.tool_name(), None);
        assert!(matches!(list, RuntimeCommand::RunList));

        let get: RuntimeCommand =
            serde_json::from_value(serde_json::json!({ "kind": "run.get", "run_id": "abc123" }))
                .expect("parse run.get");
        assert_eq!(get.name(), "run.get");
        assert_eq!(get.tool_name(), None);
        match get {
            RuntimeCommand::RunGet { run_id } => assert_eq!(run_id, "abc123"),
            other => panic!("unexpected: {}", other.name()),
        }

        for name in ["run.list", "run.get"] {
            assert!(RUNTIME_COMMAND_NAMES.contains(&name), "{name} missing");
        }
    }

    #[test]
    fn delegate_steer_and_close_round_trip() {
        let steer: RuntimeCommand = serde_json::from_value(serde_json::json!({
            "kind": "delegate.steer",
            "session_id": "job-7",
            "message": "now run the tests",
        }))
        .expect("parse delegate.steer");
        assert_eq!(steer.name(), "delegate.steer");
        assert_eq!(steer.tool_name(), None);
        match steer {
            RuntimeCommand::DelegateSteer {
                session_id,
                message,
            } => {
                assert_eq!(session_id, "job-7");
                assert_eq!(message, "now run the tests");
            }
            other => panic!("unexpected variant: {}", other.name()),
        }

        let close: RuntimeCommand = serde_json::from_value(serde_json::json!({
            "kind": "delegate.close",
            "session_id": "job-7",
        }))
        .expect("parse delegate.close");
        assert_eq!(close.name(), "delegate.close");
        assert_eq!(close.tool_name(), None);
        match close {
            RuntimeCommand::DelegateClose { session_id } => assert_eq!(session_id, "job-7"),
            other => panic!("unexpected variant: {}", other.name()),
        }

        assert!(RUNTIME_COMMAND_NAMES.contains(&"delegate.steer"));
        assert!(RUNTIME_COMMAND_NAMES.contains(&"delegate.close"));
    }

    #[test]
    fn wechat_commands_round_trip_and_are_named() {
        // login: scan-only — bot_type omitted defaults to "3" (DEFAULT_ILINK_BOT_TYPE),
        // base_url omitted by default.
        let login: RuntimeCommand =
            serde_json::from_value(serde_json::json!({ "kind": "wechat.login" }))
                .expect("parse wechat.login with no fields");
        assert_eq!(login.name(), "wechat.login");
        assert_eq!(login.tool_name(), None);
        match &login {
            RuntimeCommand::WechatLogin { bot_type, base_url } => {
                assert_eq!(bot_type, "3", "bot_type defaults to 3 (scan-only)");
                assert_eq!(base_url, &None);
            }
            other => panic!("unexpected variant: {}", other.name()),
        }
        // An explicit bot_type still overrides the default.
        let custom: RuntimeCommand = serde_json::from_value(serde_json::json!({
            "kind": "wechat.login",
            "bot_type": "7",
        }))
        .expect("parse wechat.login with explicit bot_type");
        assert!(matches!(&custom, RuntimeCommand::WechatLogin { bot_type, .. } if bot_type == "7"));
        // base_url omitted when None (skip_serializing_if).
        let value = serde_json::to_value(&login).expect("serialize wechat.login");
        assert!(value.get("base_url").is_none());

        // start: owners + agent default + autonomy default.
        let start: RuntimeCommand = serde_json::from_value(serde_json::json!({
            "kind": "wechat.start",
            "owners": ["u_alice"],
        }))
        .expect("parse wechat.start");
        match &start {
            RuntimeCommand::WechatStart {
                owners,
                agent,
                autonomy,
            } => {
                assert_eq!(owners, &vec!["u_alice".to_string()]);
                assert_eq!(agent, "claude", "agent defaults to claude");
                assert_eq!(
                    *autonomy,
                    DelegateAutonomy::ReadOnly,
                    "defaults to read-only"
                );
            }
            other => panic!("unexpected variant: {}", other.name()),
        }

        // stop / status are unit variants.
        let stop: RuntimeCommand =
            serde_json::from_value(serde_json::json!({ "kind": "wechat.stop" }))
                .expect("parse wechat.stop");
        assert_eq!(stop.name(), "wechat.stop");
        let status: RuntimeCommand =
            serde_json::from_value(serde_json::json!({ "kind": "wechat.status" }))
                .expect("parse wechat.status");
        assert_eq!(status.name(), "wechat.status");

        for name in [
            "wechat.login",
            "wechat.start",
            "wechat.stop",
            "wechat.status",
        ] {
            assert!(RUNTIME_COMMAND_NAMES.contains(&name), "{name} missing");
        }
    }

    #[test]
    fn delegate_autonomy_serde_names_and_default() {
        assert_eq!(DelegateAutonomy::default(), DelegateAutonomy::ReadOnly);
        for (autonomy, name) in [
            (DelegateAutonomy::ReadOnly, "read_only"),
            (DelegateAutonomy::Edit, "edit"),
            (DelegateAutonomy::Full, "full"),
        ] {
            assert_eq!(
                serde_json::to_value(autonomy).unwrap(),
                serde_json::json!(name)
            );
        }
    }

    #[test]
    fn approval_mode_serde_names_and_tiers() {
        for (mode, name, tier) in [
            (ApprovalMode::AlwaysAsk, "always_ask", RiskTier::ReadOnly),
            (ApprovalMode::Write, "write", RiskTier::Edit),
            (ApprovalMode::Yolo, "yolo", RiskTier::Exec),
        ] {
            assert_eq!(serde_json::to_value(mode).unwrap(), serde_json::json!(name));
            assert_eq!(mode.max_auto_tier(), tier);
        }
    }

    #[test]
    fn flow_start_round_trips_inline_workflow() {
        // An inline WorkflowDef parses through the untagged `FlowSource`.
        let value = serde_json::json!({
            "kind": "flow.start",
            "workflow": {
                "schema_version": 1,
                "name": "fan",
                "strategy": {
                    "type": "single",
                    "step": {
                        "worker": { "kind": "cli", "name": "claude" },
                        "task": "do it"
                    }
                }
            },
            "inputs": { "seed": "x" }
        });
        let command: RuntimeCommand = serde_json::from_value(value).expect("parse flow.start");
        assert_eq!(command.name(), "flow.start");
        assert_eq!(command.tool_name(), None);
        match command {
            RuntimeCommand::FlowStart {
                workflow,
                inputs,
                workspace,
            } => {
                match workflow {
                    FlowSource::Inline { workflow } => assert_eq!(workflow.name, "fan"),
                    FlowSource::Named { .. } => panic!("expected inline workflow"),
                }
                assert_eq!(inputs.unwrap().get("seed").map(String::as_str), Some("x"));
                assert_eq!(workspace, None);
            }
            other => panic!("unexpected variant: {}", other.name()),
        }
        assert!(RUNTIME_COMMAND_NAMES.contains(&"flow.start"));
    }

    #[test]
    fn flow_start_round_trips_named_reference() {
        let value = serde_json::json!({
            "kind": "flow.start",
            "workflow_ref": "triage",
        });
        let command: RuntimeCommand = serde_json::from_value(value).expect("parse flow.start ref");
        match command {
            RuntimeCommand::FlowStart { workflow, .. } => match workflow {
                FlowSource::Named { workflow_ref } => assert_eq!(workflow_ref, "triage"),
                FlowSource::Inline { .. } => panic!("expected named ref"),
            },
            other => panic!("unexpected variant: {}", other.name()),
        }
    }

    #[test]
    fn flow_steer_round_trips_with_and_without_target() {
        // Explicit node target round-trips and is on the wire.
        let value = serde_json::json!({
            "kind": "flow.steer",
            "flow_id": "f1",
            "target": { "node_id": "stage-1" },
            "message": "now run the tests",
        });
        let command: RuntimeCommand = serde_json::from_value(value).expect("parse flow.steer");
        assert_eq!(command.name(), "flow.steer");
        assert_eq!(command.tool_name(), None);
        match &command {
            RuntimeCommand::FlowSteer {
                flow_id,
                target,
                message,
            } => {
                assert_eq!(flow_id, "f1");
                assert_eq!(target, &WorkerSelector::node("stage-1"));
                assert_eq!(message, "now run the tests");
            }
            other => panic!("unexpected: {}", other.name()),
        }
        // An omitted target defaults to "the only live worker" and is skipped on
        // the wire (so the default never bloats the command value).
        let bare: RuntimeCommand = serde_json::from_value(serde_json::json!({
            "kind": "flow.steer",
            "flow_id": "f1",
            "message": "go",
        }))
        .expect("parse bare flow.steer");
        match &bare {
            RuntimeCommand::FlowSteer { target, .. } => {
                assert!(target.is_default());
                assert_eq!(target.node_id, None);
            }
            other => panic!("unexpected: {}", other.name()),
        }
        let json = serde_json::to_value(&bare).expect("serialize bare");
        assert!(
            json.get("target").is_none(),
            "default target is skipped: {json}"
        );
        assert!(RUNTIME_COMMAND_NAMES.contains(&"flow.steer"));
    }

    #[test]
    fn flow_replay_round_trips_by_flow_id_and_by_path() {
        // The common case: replay a recorded flow by id (untagged → FlowId arm).
        let value = serde_json::json!({
            "kind": "flow.replay",
            "flow_id": "job-7",
        });
        let command: RuntimeCommand = serde_json::from_value(value).expect("parse flow.replay id");
        assert_eq!(command.name(), "flow.replay");
        assert_eq!(command.tool_name(), None);
        match &command {
            RuntimeCommand::FlowReplay {
                ledger_ref,
                workspace,
            } => {
                assert_eq!(
                    ledger_ref,
                    &LedgerRef::FlowId {
                        flow_id: "job-7".into()
                    }
                );
                assert_eq!(workspace, &None);
            }
            other => panic!("unexpected: {}", other.name()),
        }

        // The explicit-path arm round-trips too.
        let by_path: RuntimeCommand = serde_json::from_value(serde_json::json!({
            "kind": "flow.replay",
            "ledger_path": "/p/.nerve/flows/job-7/ledger.jsonl",
        }))
        .expect("parse flow.replay path");
        match by_path {
            RuntimeCommand::FlowReplay { ledger_ref, .. } => assert_eq!(
                ledger_ref,
                LedgerRef::Path {
                    ledger_path: "/p/.nerve/flows/job-7/ledger.jsonl".into()
                }
            ),
            other => panic!("unexpected: {}", other.name()),
        }
        assert!(RUNTIME_COMMAND_NAMES.contains(&"flow.replay"));
    }

    #[test]
    fn flow_get_list_close_round_trip() {
        let get: RuntimeCommand =
            serde_json::from_value(serde_json::json!({ "kind": "flow.get", "flow_id": "f1" }))
                .expect("parse flow.get");
        assert_eq!(get.name(), "flow.get");
        match get {
            RuntimeCommand::FlowGet { flow_id } => assert_eq!(flow_id, "f1"),
            other => panic!("unexpected: {}", other.name()),
        }

        let list: RuntimeCommand =
            serde_json::from_value(serde_json::json!({ "kind": "flow.list" }))
                .expect("parse flow.list");
        assert_eq!(list.name(), "flow.list");
        assert!(matches!(list, RuntimeCommand::FlowList));

        let close: RuntimeCommand =
            serde_json::from_value(serde_json::json!({ "kind": "flow.close", "flow_id": "f1" }))
                .expect("parse flow.close");
        assert_eq!(close.name(), "flow.close");
        match close {
            RuntimeCommand::FlowClose { flow_id } => assert_eq!(flow_id, "f1"),
            other => panic!("unexpected: {}", other.name()),
        }
        for name in ["flow.get", "flow.list", "flow.close"] {
            assert!(RUNTIME_COMMAND_NAMES.contains(&name));
        }
    }

    #[test]
    fn flow_respond_reuses_session_approval_decision() {
        let value = serde_json::json!({
            "kind": "flow.respond",
            "flow_id": "f1",
            "request_id": "approval-3",
            "decision": "allow_always",
        });
        let command: RuntimeCommand = serde_json::from_value(value).expect("parse flow.respond");
        assert_eq!(command.name(), "flow.respond");
        match command {
            RuntimeCommand::FlowRespond {
                flow_id,
                request_id,
                decision,
            } => {
                assert_eq!(flow_id, "f1");
                assert_eq!(request_id, "approval-3");
                assert_eq!(decision, SessionApprovalDecision::AllowAlways);
            }
            other => panic!("unexpected: {}", other.name()),
        }
        assert!(RUNTIME_COMMAND_NAMES.contains(&"flow.respond"));
    }

    #[test]
    fn approval_decision_helpers_and_serde() {
        use SessionApprovalDecision::*;
        assert!(Allow.allows() && AllowAlways.allows());
        assert!(!Deny.allows() && !DenyAlways.allows());
        assert!(AllowAlways.remember() && DenyAlways.remember());
        assert!(!Allow.remember() && !Deny.remember());
        assert_eq!(
            serde_json::to_value(AllowAlways).unwrap(),
            serde_json::json!("allow_always")
        );
        assert_eq!(
            serde_json::to_value(DenyAlways).unwrap(),
            serde_json::json!("deny_always")
        );
    }

    #[test]
    fn host_capabilities_round_trips() {
        let command: RuntimeCommand =
            serde_json::from_value(serde_json::json!({ "kind": "host.capabilities" }))
                .expect("parse host.capabilities");
        assert_eq!(command.name(), "host.capabilities");
        assert_eq!(command.tool_name(), None);
        assert!(matches!(command, RuntimeCommand::HostCapabilities));
        assert!(RUNTIME_COMMAND_NAMES.contains(&"host.capabilities"));
    }

    #[test]
    fn host_clipboard_write_text_round_trips() {
        let command: RuntimeCommand = serde_json::from_value(serde_json::json!({
            "kind": "host.clipboard.write_text",
            "text": "copy me"
        }))
        .expect("parse host.clipboard.write_text");
        assert_eq!(command.name(), "host.clipboard.write_text");
        assert_eq!(command.tool_name(), None);
        match command {
            RuntimeCommand::HostClipboardWriteText { text } => assert_eq!(text, "copy me"),
            other => panic!("unexpected: {}", other.name()),
        }
        assert!(RUNTIME_COMMAND_NAMES.contains(&"host.clipboard.write_text"));
    }

    #[test]
    fn host_notification_show_round_trips() {
        let command: RuntimeCommand = serde_json::from_value(serde_json::json!({
            "kind": "host.notification.show",
            "title": "Nerve",
            "body": "Done"
        }))
        .expect("parse host.notification.show");
        assert_eq!(command.name(), "host.notification.show");
        assert_eq!(command.tool_name(), None);
        match command {
            RuntimeCommand::HostNotificationShow { title, body } => {
                assert_eq!(title, "Nerve");
                assert_eq!(body.as_deref(), Some("Done"));
            }
            other => panic!("unexpected: {}", other.name()),
        }
        assert!(RUNTIME_COMMAND_NAMES.contains(&"host.notification.show"));
    }

    #[test]
    fn host_folder_pick_round_trips() {
        let command: RuntimeCommand = serde_json::from_value(serde_json::json!({
            "kind": "host.folder.pick",
            "title": "Choose project folder"
        }))
        .expect("parse host.folder.pick");
        assert_eq!(command.name(), "host.folder.pick");
        assert_eq!(command.tool_name(), None);
        match command {
            RuntimeCommand::HostFolderPick { title } => {
                assert_eq!(title.as_deref(), Some("Choose project folder"));
            }
            other => panic!("unexpected: {}", other.name()),
        }
        assert!(RUNTIME_COMMAND_NAMES.contains(&"host.folder.pick"));
    }

    #[test]
    fn host_file_save_text_round_trips() {
        let command: RuntimeCommand = serde_json::from_value(serde_json::json!({
            "kind": "host.file.save_text",
            "title": "Save packet",
            "default_name": "packet.md",
            "text": "# Packet"
        }))
        .expect("parse host.file.save_text");
        assert_eq!(command.name(), "host.file.save_text");
        assert_eq!(command.tool_name(), None);
        match command {
            RuntimeCommand::HostFileSaveText {
                title,
                default_name,
                text,
            } => {
                assert_eq!(title.as_deref(), Some("Save packet"));
                assert_eq!(default_name.as_deref(), Some("packet.md"));
                assert_eq!(text, "# Packet");
            }
            other => panic!("unexpected: {}", other.name()),
        }
        assert!(RUNTIME_COMMAND_NAMES.contains(&"host.file.save_text"));
    }

    #[test]
    fn host_url_open_round_trips() {
        let command: RuntimeCommand = serde_json::from_value(serde_json::json!({
            "kind": "host.url.open",
            "url": "https://example.com/auth"
        }))
        .expect("parse host.url.open");
        assert_eq!(command.name(), "host.url.open");
        assert_eq!(command.tool_name(), None);
        match command {
            RuntimeCommand::HostUrlOpen { url } => assert_eq!(url, "https://example.com/auth"),
            other => panic!("unexpected: {}", other.name()),
        }
        assert!(RUNTIME_COMMAND_NAMES.contains(&"host.url.open"));
    }

    #[test]
    fn workspace_reveal_round_trips() {
        let bare: RuntimeCommand =
            serde_json::from_value(serde_json::json!({ "kind": "workspace.reveal" }))
                .expect("parse workspace.reveal");
        assert_eq!(bare.name(), "workspace.reveal");
        assert_eq!(bare.tool_name(), None);
        match bare {
            RuntimeCommand::WorkspaceReveal { workspace } => assert_eq!(workspace, None),
            other => panic!("unexpected: {}", other.name()),
        }
        let with: RuntimeCommand = serde_json::from_value(
            serde_json::json!({ "kind": "workspace.reveal", "workspace": "main" }),
        )
        .expect("parse with workspace");
        match with {
            RuntimeCommand::WorkspaceReveal { workspace } => {
                assert_eq!(workspace.as_deref(), Some("main"));
            }
            other => panic!("unexpected: {}", other.name()),
        }
        assert!(RUNTIME_COMMAND_NAMES.contains(&"workspace.reveal"));
    }
}
