//! Outcome-corpus vocabulary — the **L6 ground-truth labels** that close the
//! trust loop (`docs/designs/trust-substrate.md` §3 L6). After a run is captured
//! (L0), gated by a verdict (L2) and a signed receipt (L4), the org records what
//! *actually happened* to the change: was it [`Outcome::Merged`], later
//! [`Outcome::Reverted`], implicated in an [`Outcome::Incident`], or
//! [`Outcome::ShippedNoRegress`]. These labels are the **non-load-bearing**
//! observational corpus that calibration (deferred) reads from — never an input
//! to a verdict (INV-R1: Nerve is a court reporter, not a judge; INV-R3/R4: the
//! label is advisory, it does not change what cleared the bar).
//!
//! These are **pure, transport-neutral serde data** with **no behavior** — every
//! hash field is a plain `String` filled by the pure helpers in
//! `nerve-core::outcome` (INV-R2: the hashing is pure and golden-tested), never
//! here. Each [`OutcomeRecord`] carries an append-only [`OutcomeLabel`] tape whose
//! per-label digest chains into the previous one
//! (`chained[n] = sha256(chained[n-1] || label_hash[n])`), mirroring the L0
//! provenance spine so the corpus is tamper-evident.
//!
//! **No floats** appear in any hashed type: `seq` / `*_at_ms` counts are `u64`, so
//! the canonical JSON is byte-stable and the types derive `Eq` — exact golden
//! snapshots, no precision or `-0.0`/NaN nondeterminism (INV-R2). `observed_at_ms`
//! is host wall-clock metadata, **excluded from the hashed bytes**.

#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// On-disk + on-wire outcome-corpus schema version. Bumped only for additive,
/// backward-compatible changes to the [`OutcomeRecord`] shape; a reader rejects a
/// record from a newer major it cannot understand rather than silently dropping
/// fields.
pub const OUTCOME_SCHEMA_VERSION: u32 = 1;

/// The ground-truth disposition of a captured change (`trust-substrate.md` §3 L6).
/// Internally tagged on `outcome` (`{"outcome": "..."}`) so a label's disposition
/// is golden-diffable and additive (new dispositions may be appended). This is an
/// **observation**, not a judgment — it records what the org saw happen, never
/// whether the code was "correct" (INV-R1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// The change landed on the target branch.
    Merged,
    /// A previously merged change was rolled back.
    Reverted,
    /// The change was implicated in a production incident.
    Incident,
    /// The change shipped and produced no observed regression.
    ShippedNoRegress,
}

/// Where an [`OutcomeLabel`] came from (`trust-substrate.md` §3 L6). Internally
/// tagged on `source` (`{"source": "..."}`). Provenance of the *label*, distinct
/// from the [`Outcome`] it asserts — a human disposition and a CI-derived one are
/// the same disposition from different witnesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum LabelSource {
    /// A person recorded the disposition (e.g. a reviewer marking a revert).
    Human,
    /// A CI signal recorded it (e.g. the merge pipeline).
    Ci,
    /// Passive telemetry / monitoring inferred it (e.g. an incident alert).
    Observation,
}

/// One entry on a run's append-only outcome tape: a logical sequence number, the
/// observed [`Outcome`], its [`LabelSource`], and tamper-evident chaining hashes.
/// `seq` is a monotonic logical clock (0,1,2,…) assigned at append, *not* a
/// wall-clock — so a replay reproduces byte-identical ordering and hashes.
/// `label_hash` is this label's own digest and `chained_hash` commits to it plus
/// every prior label; both are filled by `nerve-core::outcome`. `observed_at_ms`
/// is display-only host metadata and is **excluded from the hashed bytes**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct OutcomeLabel {
    /// 0-based logical position of this label within its [`OutcomeRecord`] tape.
    pub seq: u64,
    /// The observed disposition of the change.
    pub outcome: Outcome,
    /// Where this label came from.
    pub source: LabelSource,
    /// Optional actor that recorded the label (a username, bot id, or monitor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// Optional free-text note attached to the label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Optional reference to the verdict this disposition relates to (a
    /// `verdict_id`); kept a plain `Option<String>` so the corpus carries no hard
    /// type dependency on the verdict layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict_ref: Option<String>,
    /// Host wall-clock instant the label was observed (display metadata; excluded
    /// from the hashed bytes so it never perturbs the content address).
    pub observed_at_ms: u64,
    /// This label's own content digest (filled by `nerve-core::outcome`).
    #[serde(default)]
    pub label_hash: String,
    /// The chained digest committing to this label and every prior one.
    #[serde(default)]
    pub chained_hash: String,
}

/// The outcome corpus for a single captured run: the append-only [`Self::labels`]
/// tape plus its content-addressed [`Self::labels_root`] (the spine head, `""` for
/// an empty tape). Optional `session_id` / `agent` / `receipt_id` denormalize the
/// run's identity for query without a join; all are plain `Option<String>` so the
/// corpus has no compile-time dependency on the receipt or session layers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct OutcomeRecord {
    /// Additive schema version of this record (see [`OUTCOME_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// The captured run these labels are about (the L0 `run_id`).
    pub run_id: String,
    /// Optional originating session id (denormalized for query).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Optional agent that produced the run (denormalized for query).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Optional receipt issued for the run (denormalized for query).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<String>,
    /// The ordered, append-only label tape.
    #[serde(default)]
    pub labels: Vec<OutcomeLabel>,
    /// The spine head committing to the whole label tape (`""` when empty).
    #[serde(default)]
    pub labels_root: String,
}

/// A rolled-up count over a set of [`OutcomeRecord`]s — the corpus dashboard
/// figures. `labeled_runs` counts runs with at least one label; the four
/// disposition counts tally labels by [`Outcome`]. All `u64`, no floats, so the
/// summary is byte-stable and the type derives `Eq`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct OutcomeSummary {
    /// Total runs considered.
    pub total_runs: u64,
    /// Runs carrying at least one label.
    pub labeled_runs: u64,
    /// Count of [`Outcome::Merged`] labels.
    pub merged: u64,
    /// Count of [`Outcome::Reverted`] labels.
    pub reverted: u64,
    /// Count of [`Outcome::Incident`] labels.
    pub incident: u64,
    /// Count of [`Outcome::ShippedNoRegress`] labels.
    pub shipped_no_regress: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_tags_are_snake_case() {
        let cases = [
            (Outcome::Merged, "merged"),
            (Outcome::Reverted, "reverted"),
            (Outcome::Incident, "incident"),
            (Outcome::ShippedNoRegress, "shipped_no_regress"),
        ];
        for (outcome, tag) in cases {
            let value = serde_json::to_value(outcome).expect("outcome json");
            assert_eq!(value["outcome"], tag);
        }
    }

    #[test]
    fn label_source_tags_are_snake_case() {
        let cases = [
            (LabelSource::Human, "human"),
            (LabelSource::Ci, "ci"),
            (LabelSource::Observation, "observation"),
        ];
        for (source, tag) in cases {
            let value = serde_json::to_value(source).expect("source json");
            assert_eq!(value["source"], tag);
        }
    }

    #[test]
    fn outcome_label_omits_optionals_and_round_trips() {
        let label = OutcomeLabel {
            seq: 0,
            outcome: Outcome::Merged,
            source: LabelSource::Human,
            actor: None,
            note: None,
            verdict_ref: None,
            observed_at_ms: 1234,
            label_hash: String::new(),
            chained_hash: String::new(),
        };
        let value = serde_json::to_value(&label).expect("label json");
        assert_eq!(value["outcome"]["outcome"], "merged");
        assert_eq!(value["source"]["source"], "human");
        assert_eq!(value["seq"], 0);
        assert_eq!(value["observed_at_ms"], 1234);
        assert!(value.get("actor").is_none());
        assert!(value.get("note").is_none());
        assert!(value.get("verdict_ref").is_none());
        let back: OutcomeLabel = serde_json::from_value(value).expect("round-trip");
        assert_eq!(back, label);
    }

    #[test]
    fn outcome_label_keeps_present_optionals() {
        let label = OutcomeLabel {
            seq: 2,
            outcome: Outcome::Reverted,
            source: LabelSource::Ci,
            actor: Some("ci-bot".into()),
            note: Some("rolled back".into()),
            verdict_ref: Some("v-9".into()),
            observed_at_ms: 99,
            label_hash: " aa".trim().into(),
            chained_hash: "bb".into(),
        };
        let value = serde_json::to_value(&label).expect("label json");
        assert_eq!(value["actor"], "ci-bot");
        assert_eq!(value["note"], "rolled back");
        assert_eq!(value["verdict_ref"], "v-9");
        assert_eq!(value["label_hash"], "aa");
        assert_eq!(value["chained_hash"], "bb");
        let back: OutcomeLabel = serde_json::from_value(value).expect("round-trip");
        assert_eq!(back, label);
    }

    #[test]
    fn outcome_record_round_trips_and_defaults_are_tolerant() {
        let record = OutcomeRecord {
            schema_version: OUTCOME_SCHEMA_VERSION,
            run_id: "run-1".into(),
            session_id: Some("job-3".into()),
            agent: Some("codex".into()),
            receipt_id: Some("rcpt-7".into()),
            labels: vec![OutcomeLabel {
                seq: 0,
                outcome: Outcome::ShippedNoRegress,
                source: LabelSource::Observation,
                actor: None,
                note: None,
                verdict_ref: None,
                observed_at_ms: 10,
                label_hash: "ff".into(),
                chained_hash: "ee".into(),
            }],
            labels_root: "ee".into(),
        };
        let value = serde_json::to_value(&record).expect("record json");
        let back: OutcomeRecord = serde_json::from_value(value).expect("round-trip");
        assert_eq!(back, record);

        // A minimal record (only the non-default fields) deserializes, with the
        // additive fields falling back to their defaults — forward tolerance.
        let minimal: OutcomeRecord = serde_json::from_value(serde_json::json!({
            "schema_version": OUTCOME_SCHEMA_VERSION,
            "run_id": "x",
        }))
        .expect("minimal record");
        assert_eq!(minimal.session_id, None);
        assert_eq!(minimal.agent, None);
        assert_eq!(minimal.receipt_id, None);
        assert!(minimal.labels.is_empty());
        assert_eq!(minimal.labels_root, "");
    }

    #[test]
    fn outcome_summary_defaults_to_zero_and_round_trips() {
        let summary = OutcomeSummary::default();
        assert_eq!(summary.total_runs, 0);
        assert_eq!(summary.labeled_runs, 0);
        assert_eq!(summary.merged, 0);
        assert_eq!(summary.reverted, 0);
        assert_eq!(summary.incident, 0);
        assert_eq!(summary.shipped_no_regress, 0);
        let value = serde_json::to_value(&summary).expect("summary json");
        let back: OutcomeSummary = serde_json::from_value(value).expect("round-trip");
        assert_eq!(back, summary);
    }
}
