//! Policy-as-code vocabulary — the **L3 policy plane** data shapes
//! (`docs/designs/trust-substrate.md` §3 L3, §5). A [`PolicyDoc`] is the org's
//! sealed, content-addressed statement of *what a delegated agent may do* (the
//! [`Capability`] grants per tool/action via [`CapabilityRule`]) and *what bar a
//! change must clear* (the [`MergeBar`]'s required checks + [`EvidenceRequirement`]s).
//! Every gate decision the host makes against that doc is captured as a
//! [`PolicyDecisionRecord`] so the evidence ledger can prove the policy was
//! actually exercised.
//!
//! **Court reporter, not judge (INV-R1):** these shapes *record* a decision the
//! org's policy implied; they never assert the underlying change is "correct".
//!
//! Like the L0 provenance shapes these are **pure, transport-neutral serde data**
//! with **no behavior** — the hash fields (`args_hash`) are plain `String`s filled
//! by the pure SHA-256 helpers in `nerve-core::policy` (INV-R2: hashing is pure and
//! golden-tested), never here. This crate only names the shapes so they are
//! wasm-shareable and appear in the exported protocol schema.
//!
//! **No floats** appear anywhere, so the canonical JSON is byte-stable and the
//! types derive `Eq` — exact golden snapshots, no precision nondeterminism
//! (INV-R2).

#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// On-disk + on-wire policy schema version. Bumped only for additive,
/// backward-compatible changes to the [`PolicyDoc`] shape (mirrors
/// [`crate::provenance::RUN_SCHEMA_VERSION`]); a reader rejects a record from a
/// newer major it cannot understand rather than silently dropping fields.
pub const POLICY_SCHEMA_VERSION: u32 = 1;

/// A coarse capability class a [`CapabilityRule`] grants to a delegated agent's
/// tool/action (`trust-substrate.md` §3 L3). The classes are deliberately broad —
/// the policy plane is a *coarse* admission gate, not a fine-grained sandbox (that
/// hermetic-isolation job belongs to L2's `SandboxLauncher`). `Exec` is the
/// [`Default`] because an unclassified action is treated as the most consequential
/// (executing code) until policy says otherwise — fail-closed. Additive: new
/// classes may be appended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Read repository / filesystem state without mutating it.
    Read,
    /// Mutate repository / filesystem state (edits, writes).
    Write,
    /// Reach the network / send data outbound (egress).
    Egress,
    /// Execute code / spawn processes — the fail-closed default.
    #[default]
    Exec,
}

/// One admission rule in a [`PolicyDoc`]: the [`Capability`] this `tool`/`action`
/// pair is granted, optionally scoped to a single `agent`. The host resolves an
/// incoming tool call against the ordered rules to classify it before recording a
/// [`PolicyDecisionRecord`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct CapabilityRule {
    /// The tool this rule applies to (e.g. the MCP tool name).
    pub tool: String,
    /// The action within the tool this rule applies to.
    pub action: String,
    /// The capability class granted — defaults to [`Capability::Exec`].
    #[serde(default)]
    pub capability: Capability,
    /// Optionally scope the rule to a single agent; `None` = any agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// The bar a change must clear to merge: the names of the required checks (matched
/// against the L2 verdict's [`crate::verdict::CheckResult::name`]s). An empty list
/// means no required checks are declared — the gate exercises no bar.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct MergeBar {
    /// Names of the checks that must pass for a change to clear the bar.
    #[serde(default)]
    pub required_checks: Vec<String>,
}

impl MergeBar {
    /// Whether the bar declares no required checks (the empty bar exercises nothing).
    /// Used by [`crate::receipt::ReceiptStatement`]'s additive `skip_serializing_if`
    /// so a receipt sealed without an org bar serializes byte-identically to a
    /// pre-L3 receipt (additive-invariance) — the empty `merge_bar` key is omitted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.required_checks.is_empty()
    }
}

/// A required piece of evidence a [`PolicyDoc`] demands beyond the merge-bar checks
/// (e.g. a signed receipt, a replay manifest). `kind` is a free-form discriminator
/// the host interprets; kept minimal and additive so the policy vocabulary can grow
/// without a schema bump.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct EvidenceRequirement {
    /// The evidence discriminator (host-interpreted).
    pub kind: String,
}

/// The org's sealed policy-as-code document — the L3 unit of trust. It declares the
/// per-tool [`CapabilityRule`]s, the [`MergeBar`], and any [`EvidenceRequirement`]s.
/// `policy_version` is a content address stamped by `nerve-core::policy::seal_policy`
/// (it is zeroed before hashing the rest, then filled with the digest), so the same
/// policy body always yields the same version. [`Default`] yields the empty
/// (deny-by-default) policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct PolicyDoc {
    /// Policy schema version — see [`POLICY_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Content-addressed policy version, stamped at seal time (empty until sealed).
    #[serde(default)]
    pub policy_version: String,
    /// The per-tool/action capability grants.
    #[serde(default)]
    pub capabilities: Vec<CapabilityRule>,
    /// The merge bar a change must clear.
    #[serde(default)]
    pub merge_bar: MergeBar,
    /// Additional evidence the policy requires.
    #[serde(default)]
    pub required_evidence: Vec<EvidenceRequirement>,
}

/// One captured policy gate decision: the host classified an agent's tool call
/// against the [`PolicyDoc`] (`policy_version`) and `allow`/`deny`'d it. `args_hash`
/// is the content address of the (canonicalized) call arguments, filled by
/// `nerve-core::policy::hash_args` — the raw args are not stored here, only the
/// digest, so the record is portable and privacy-preserving. The evidence ledger
/// (L1) commits to these so the policy's exercise is provable (INV-R1: recording a
/// decision, not judging the change).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct PolicyDecisionRecord {
    /// Policy schema version — see [`POLICY_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The [`PolicyDoc::policy_version`] this decision was made against.
    pub policy_version: String,
    /// The delegate session this decision belongs to.
    pub session_id: String,
    /// The agent whose call was gated (empty if unattributed).
    #[serde(default)]
    pub agent: String,
    /// The tool that was called.
    pub tool: String,
    /// The capability class the call was classified as.
    pub capability: Capability,
    /// The verdict: `allow` or `deny` (free-form, host-interpreted).
    pub decision: String,
    /// Human-readable reason for the decision.
    pub reason: String,
    /// Content address of the canonicalized call arguments (empty if none).
    #[serde(default)]
    pub args_hash: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_variants_are_snake_case() {
        let cases = [
            (Capability::Read, "read"),
            (Capability::Write, "write"),
            (Capability::Egress, "egress"),
            (Capability::Exec, "exec"),
        ];
        for (cap, tag) in cases {
            let value = serde_json::to_value(cap).expect("capability json");
            assert_eq!(value, serde_json::Value::String(tag.into()));
            let back: Capability = serde_json::from_value(value).expect("round-trip");
            assert_eq!(back, cap);
        }
    }

    #[test]
    fn capability_default_is_exec() {
        // Fail-closed: an unclassified action is the most consequential one.
        assert_eq!(Capability::default(), Capability::Exec);
    }

    #[test]
    fn capability_rule_omits_optional_agent_and_defaults_capability() {
        let rule = CapabilityRule {
            tool: "edit".into(),
            action: "write".into(),
            capability: Capability::Write,
            agent: None,
        };
        let value = serde_json::to_value(&rule).expect("rule json");
        assert_eq!(value["tool"], "edit");
        assert_eq!(value["action"], "write");
        assert_eq!(value["capability"], "write");
        assert!(value.get("agent").is_none());
        let back: CapabilityRule = serde_json::from_value(value).expect("round-trip");
        assert_eq!(back, rule);

        // A minimal rule (no capability, no agent) falls back to the Exec default.
        let minimal: CapabilityRule = serde_json::from_value(serde_json::json!({
            "tool": "shell",
            "action": "run",
        }))
        .expect("minimal rule");
        assert_eq!(minimal.capability, Capability::Exec);
        assert_eq!(minimal.agent, None);
    }

    #[test]
    fn policy_doc_round_trips_and_defaults_are_tolerant() {
        let doc = PolicyDoc {
            schema_version: POLICY_SCHEMA_VERSION,
            policy_version: "abc123".into(),
            capabilities: vec![CapabilityRule {
                tool: "read_file".into(),
                action: "read".into(),
                capability: Capability::Read,
                agent: Some("codex".into()),
            }],
            merge_bar: MergeBar {
                required_checks: vec!["unit".into(), "build".into()],
            },
            required_evidence: vec![EvidenceRequirement {
                kind: "receipt".into(),
            }],
        };
        let value = serde_json::to_value(&doc).expect("doc json");
        let back: PolicyDoc = serde_json::from_value(value).expect("round-trip");
        assert_eq!(back, doc);

        // A minimal record (only schema_version) deserializes, with the additive
        // fields falling back to their defaults — forward tolerance.
        let minimal: PolicyDoc = serde_json::from_value(serde_json::json!({
            "schema_version": POLICY_SCHEMA_VERSION,
        }))
        .expect("minimal doc");
        assert_eq!(minimal.policy_version, "");
        assert!(minimal.capabilities.is_empty());
        assert!(minimal.merge_bar.required_checks.is_empty());
        assert!(minimal.required_evidence.is_empty());

        // PolicyDoc::default() is the empty (deny-by-default) policy.
        let empty = PolicyDoc::default();
        assert_eq!(empty.policy_version, "");
        assert!(empty.capabilities.is_empty());
    }

    #[test]
    fn policy_decision_record_round_trips_and_omits_empty_args_hash() {
        let record = PolicyDecisionRecord {
            schema_version: POLICY_SCHEMA_VERSION,
            policy_version: "pv1".into(),
            session_id: "job-7".into(),
            agent: "claude".into(),
            tool: "edit".into(),
            capability: Capability::Write,
            decision: "allow".into(),
            reason: "matched write rule".into(),
            args_hash: "ff".into(),
        };
        let value = serde_json::to_value(&record).expect("record json");
        assert_eq!(value["capability"], "write");
        assert_eq!(value["decision"], "allow");
        let back: PolicyDecisionRecord = serde_json::from_value(value).expect("round-trip");
        assert_eq!(back, record);

        // A minimal record: empty agent + empty args_hash fall back to defaults.
        let minimal: PolicyDecisionRecord = serde_json::from_value(serde_json::json!({
            "schema_version": POLICY_SCHEMA_VERSION,
            "policy_version": "pv1",
            "session_id": "s",
            "tool": "shell",
            "capability": "exec",
            "decision": "deny",
            "reason": "no rule",
        }))
        .expect("minimal record");
        assert_eq!(minimal.agent, "");
        assert_eq!(minimal.args_hash, "");
        assert_eq!(minimal.capability, Capability::Exec);
    }
}
