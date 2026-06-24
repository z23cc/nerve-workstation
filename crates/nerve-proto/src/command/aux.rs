//! Auxiliary parameter types for [`RuntimeCommand`](super::RuntimeCommand) — the
//! untagged source/selector enums and the approval-posture enums carried as command
//! fields. Split out of `command.rs` purely to keep that file under the file-size
//! convention; these are re-exported (`pub use aux::*`) so `crate::command::*` and
//! the `nerve_proto::*` re-exports resolve unchanged.

use crate::{RiskTier, WorkflowDef};
#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The workflow a [`RuntimeCommand::FlowStart`](super::RuntimeCommand::FlowStart)
/// runs: either an **inline** [`WorkflowDef`] or a **named reference** to a loaded
/// `WorkflowDef` data file (design §4 / §6, the P3 workflow-defs surface). Untagged
/// so a client can send either `{ "workflow": { ... } }` or `{ "workflow_ref":
/// "name" }` inside the `flow.start` command without an extra discriminant.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(untagged)]
pub enum FlowSource {
    /// An inline workflow definition (the whole strategy as data). Boxed so the
    /// large inline-def variant doesn't bloat every command value (the named-ref
    /// variant is tiny).
    Inline { workflow: Box<WorkflowDef> },
    /// A named workflow resolved from a loaded data file (loaded, not compiled).
    Named { workflow_ref: String },
}

/// The OTel-GenAI trace a [`RuntimeCommand::OtelIngest`](super::RuntimeCommand::OtelIngest)
/// reconstructs a `Partial` run from (L5). Untagged so a client sends either an
/// inline trace object or a filesystem path, without an extra discriminant.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(untagged)]
pub enum OtelSource {
    /// An inline OTel trace payload (a spans document).
    Inline { trace: Value },
    /// A filesystem path to an OTel trace export.
    Path { trace_path: String },
}

/// Which recorded ledger a [`RuntimeCommand::FlowReplay`](super::RuntimeCommand::FlowReplay)
/// replays (design §4/§5). Untagged so a client sends either `{ "flow_id": "job-7" }`
/// (resolve the ledger from the `FlowStore` by flow id — the common case) or
/// `{ "ledger_path": "…" }` (an explicit `.nerve/flows/<id>/ledger.jsonl`-shaped
/// path), without an extra discriminant. Deliberately MINIMAL — a closed two-arm
/// enum, not a query language.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(untagged)]
pub enum LedgerRef {
    /// Resolve the recorded ledger from the `FlowStore` by its `flow_id`.
    FlowId { flow_id: String },
    /// Replay a ledger at an explicit filesystem path (e.g. a copied tape).
    Path { ledger_path: String },
}

/// Which live branch a [`RuntimeCommand::FlowSteer`](super::RuntimeCommand::FlowSteer)
/// targets (design §4, the `WorkerSelector` row). Deliberately MINIMAL: a flow branch
/// is identified by its deterministic node id, or — when `node_id` is unset — "the
/// single live worker" (the common case for a `Single`/`Pipeline` flow, which has
/// exactly one live branch at a time). An ambiguous unset selector against a
/// multi-branch live flow is refused by the host with a clear message; the closed
/// enum keeps the steer surface from drifting into a query language.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct WorkerSelector {
    /// The deterministic node id of the branch to steer (e.g. `"node-0"` for a
    /// `Single` flow, `"stage-1"` for a pipeline's second stage). `None` targets
    /// the only live worker, erroring if more than one is live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
}

impl WorkerSelector {
    /// A selector targeting a specific node id.
    #[must_use]
    pub fn node(node_id: impl Into<String>) -> Self {
        Self {
            node_id: Some(node_id.into()),
        }
    }

    /// Whether this is the default ("only live worker") selector — used to keep an
    /// unset `target` off the wire (serde `skip_serializing_if`).
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.node_id.is_none()
    }
}

/// Decision supplied by a human/client for a session approval request.
///
/// `Allow`/`Deny` apply to this call only; `AllowAlways`/`DenyAlways` additionally
/// signal the host to remember the decision for future calls (P2 wires the
/// remembering; P1 only distinguishes allow-vs-deny via [`Self::allows`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SessionApprovalDecision {
    /// Allow this call only.
    Allow,
    /// Deny this call only.
    Deny,
    /// Allow this call and remember the allow for future matching calls.
    AllowAlways,
    /// Deny this call and remember the deny for future matching calls.
    DenyAlways,
}

impl SessionApprovalDecision {
    /// Whether the decision permits the call (either the one-shot or remembered
    /// allow). Consumers should compare with this rather than `== Allow` so the
    /// remembered variant is not silently treated as a deny.
    #[must_use]
    pub fn allows(&self) -> bool {
        matches!(self, Self::Allow | Self::AllowAlways)
    }

    /// Whether the host should persist this decision for future matching calls.
    #[must_use]
    pub fn remember(&self) -> bool {
        matches!(self, Self::AllowAlways | Self::DenyAlways)
    }
}

/// Per-session approval posture controlling how high a [`RiskTier`] the gate may
/// auto-approve without prompting. Pure protocol data; the host gate (P2) maps
/// each tool's tier against [`Self::max_auto_tier`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Prompt for everything above read-only.
    AlwaysAsk,
    /// Auto-approve reads and edits; prompt for exec.
    Write,
    /// Auto-approve everything, including exec.
    Yolo,
}

impl ApprovalMode {
    /// Highest tier this mode auto-approves without prompting: anything at or
    /// below it is allowed, anything above it requires an approval round-trip.
    #[must_use]
    pub fn max_auto_tier(self) -> RiskTier {
        match self {
            Self::AlwaysAsk => RiskTier::ReadOnly,
            Self::Write => RiskTier::Edit,
            Self::Yolo => RiskTier::Exec,
        }
    }
}
