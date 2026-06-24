//! Pure, golden-tested content-addressing for the L6 **outcome corpus**
//! (`docs/designs/trust-substrate.md` §8, INV-R1/R3). Outcome labels are the
//! *ground-truth signal* a run accrues after the fact — was it `merged`,
//! `reverted`, did it cause an `incident`, did it ship without regression —
//! attached by a human, by CI, or by observation. They are **advisory and
//! non-load-bearing** (INV-R1: Nerve is a court reporter, never a judge), but
//! they must still be tamper-evident so the corpus that calibration (deferred,
//! INV-R3/R4) eventually reads cannot be silently rewritten.
//!
//! This module seals an ordered [`OutcomeLabel`] sequence into a content-addressed
//! chain mirroring the L0 [`crate::provenance`] spine: each label is SHA-256 hashed
//! over its canonical JSON (**excluding** `label_hash`, `chained_hash`, and the
//! host-supplied `observed_at_ms`), and the per-label digests are folded into a
//! linear hash chain whose head ([`OutcomeRecord::labels_root`]) is the single
//! content address committing to the whole labelling history of a run.
//!
//! **Determinism boundary (INV-R2):** every function here is a pure function of its
//! arguments — no IO, no wall-clock, no randomness — so the same labels in yield a
//! byte-identical chain out. Wall-clock (`observed_at_ms`) is host metadata carried
//! for display and is **never** hashed, so timestamps never perturb the chain.
//! Persistence (which DOES touch the world) lives above the kernel in
//! `nerve-workstation`.

// Re-export the shapes this module content-addresses so a consumer of the kernel
// builds and reads back an outcome corpus through `nerve_core` alone.
pub use nerve_proto::outcome::{
    LabelSource, OUTCOME_SCHEMA_VERSION, Outcome, OutcomeLabel, OutcomeRecord, OutcomeSummary,
};
use sha2::{Digest, Sha256};

/// Lowercase-hex SHA-256 of one label's *identity* — its content **excluding** the
/// derived `label_hash`/`chained_hash` and the host-supplied `observed_at_ms`. The
/// digest therefore commits to `seq`, `outcome`, `source`, `actor`, `note`, and
/// `verdict_ref` only, so re-sealing the same label at a different wall-clock yields
/// the same identity hash (INV-R2).
#[must_use]
pub fn hash_label(label: &OutcomeLabel) -> String {
    // Hash a normalized clone with the derived/wall-clock fields zeroed, so the
    // serialized bytes depend solely on the label's intrinsic content.
    let identity = OutcomeLabel {
        observed_at_ms: 0,
        label_hash: String::new(),
        chained_hash: String::new(),
        ..label.clone()
    };
    let bytes = serde_json::to_vec(&identity).expect("OutcomeLabel serializes infallibly");
    hex(Sha256::digest(bytes).as_slice())
}

/// Seal an ordered label sequence into its content-addressed chain. For each label,
/// `label_hash = hash_label(label)` and
/// `chained_hash[n] = sha256(chained_hash[n-1] || label_hash[n])` with
/// `chained_hash[-1] = ""`; the returned root is `chained_hash[last]` (`""` for an
/// empty sequence). Each returned [`OutcomeLabel`] carries both derived digests so a
/// verifier can re-derive — and thus detect tampering of — the chain.
#[must_use]
pub fn seal_labels(labels: Vec<OutcomeLabel>) -> (Vec<OutcomeLabel>, String) {
    let mut sealed = Vec::with_capacity(labels.len());
    let mut prev = String::new();
    for label in labels {
        let label_hash = hash_label(&label);
        let mut hasher = Sha256::new();
        hasher.update(prev.as_bytes());
        hasher.update(label_hash.as_bytes());
        let chained_hash = hex(hasher.finalize().as_slice());
        prev = chained_hash.clone();
        sealed.push(OutcomeLabel {
            label_hash,
            chained_hash,
            ..label
        });
    }
    (sealed, prev)
}

/// Append one label to a record and re-seal the chain, returning the updated record.
/// The incoming label's derived digests are recomputed by [`seal_labels`], so a
/// caller may pass a bare label (digests empty); `labels_root` is refreshed to the
/// new chain head.
#[must_use]
pub fn append_label(record: OutcomeRecord, label: OutcomeLabel) -> OutcomeRecord {
    let mut labels = record.labels;
    labels.push(label);
    let (labels, labels_root) = seal_labels(labels);
    OutcomeRecord {
        labels,
        labels_root,
        ..record
    }
}

/// Build an empty outcome record for a run, with no labels and an empty root. The
/// identifying `run_id`/`session_id`/`agent` are stamped as given; labels are
/// accrued later via [`append_label`].
#[must_use]
pub fn empty_record(
    run_id: impl Into<String>,
    session_id: Option<String>,
    agent: Option<String>,
) -> OutcomeRecord {
    OutcomeRecord {
        schema_version: OUTCOME_SCHEMA_VERSION,
        run_id: run_id.into(),
        session_id,
        agent,
        receipt_id: None,
        labels: Vec::new(),
        labels_root: String::new(),
    }
}

/// Aggregate a corpus of outcome records into an [`OutcomeSummary`]. `total_runs` is
/// the record count; `labeled_runs` counts records carrying at least one label; the
/// per-outcome tallies (`merged`/`reverted`/`incident`/`shipped_no_regress`) count
/// every label across all records (a run with two `merged` labels contributes two).
/// Purely a fold — no IO, deterministic.
#[must_use]
pub fn summarize(records: &[OutcomeRecord]) -> OutcomeSummary {
    let mut summary = OutcomeSummary {
        total_runs: records.len() as u64,
        ..OutcomeSummary::default()
    };
    for record in records {
        if !record.labels.is_empty() {
            summary.labeled_runs += 1;
        }
        for label in &record.labels {
            match label.outcome {
                Outcome::Merged => summary.merged += 1,
                Outcome::Reverted => summary.reverted += 1,
                Outcome::Incident => summary.incident += 1,
                Outcome::ShippedNoRegress => summary.shipped_no_regress += 1,
            }
        }
    }
    summary
}

/// Lowercase-hex encode bytes (mirrors [`crate::provenance`]'s `hex`).
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(seq: u64, outcome: Outcome, source: LabelSource) -> OutcomeLabel {
        OutcomeLabel {
            seq,
            outcome,
            source,
            actor: None,
            note: None,
            verdict_ref: None,
            observed_at_ms: 0,
            label_hash: String::new(),
            chained_hash: String::new(),
        }
    }

    #[test]
    fn hash_label_is_stable_and_excludes_wallclock_and_derived() {
        let base = label(0, Outcome::Merged, LabelSource::Human);
        assert_eq!(
            hash_label(&base),
            hash_label(&base),
            "same label -> same hash"
        );
        assert_eq!(hash_label(&base).len(), 64);
        assert!(hash_label(&base).chars().all(|c| c.is_ascii_hexdigit()));
        // observed_at_ms and the derived digests are excluded from the identity.
        let mut noisy = base.clone();
        noisy.observed_at_ms = 999_999;
        noisy.label_hash = "deadbeef".into();
        noisy.chained_hash = "cafe".into();
        assert_eq!(hash_label(&base), hash_label(&noisy));
        // A different intrinsic field DOES change the hash.
        let other = label(0, Outcome::Reverted, LabelSource::Human);
        assert_ne!(hash_label(&base), hash_label(&other));
        // seq is part of the identity.
        let later = label(1, Outcome::Merged, LabelSource::Human);
        assert_ne!(hash_label(&base), hash_label(&later));
    }

    #[test]
    fn seal_labels_chains_and_is_tamper_evident() {
        let labels = vec![
            label(0, Outcome::Merged, LabelSource::Ci),
            label(1, Outcome::ShippedNoRegress, LabelSource::Observation),
        ];
        let (sealed, root) = seal_labels(labels.clone());
        assert_eq!(sealed.len(), 2);
        assert!(!root.is_empty());
        assert_eq!(sealed.last().unwrap().chained_hash, root);
        assert_eq!(sealed[0].label_hash, hash_label(&labels[0]));
        // Deterministic.
        let (_, root_again) = seal_labels(labels.clone());
        assert_eq!(root, root_again);
        // Tampering with the FIRST label perturbs the head (and every later chain).
        let mut tampered = labels;
        tampered[0] = label(0, Outcome::Incident, LabelSource::Ci);
        let (_, tampered_root) = seal_labels(tampered);
        assert_ne!(root, tampered_root);
    }

    #[test]
    fn empty_sequence_yields_empty_root() {
        let (sealed, root) = seal_labels(Vec::new());
        assert!(sealed.is_empty());
        assert_eq!(root, "");
    }

    #[test]
    fn append_label_extends_chain_and_refreshes_root() {
        let record = empty_record("run-1", Some("sess-1".into()), Some("codex".into()));
        assert!(record.labels.is_empty());
        assert_eq!(record.labels_root, "");
        assert_eq!(record.schema_version, OUTCOME_SCHEMA_VERSION);

        let r1 = append_label(record, label(0, Outcome::Merged, LabelSource::Human));
        assert_eq!(r1.labels.len(), 1);
        assert_eq!(r1.labels_root, r1.labels[0].chained_hash);
        assert!(!r1.labels_root.is_empty());

        let root_after_one = r1.labels_root.clone();
        let r2 = append_label(r1, label(1, Outcome::Reverted, LabelSource::Ci));
        assert_eq!(r2.labels.len(), 2);
        assert_eq!(r2.labels_root, r2.labels[1].chained_hash);
        // The head moves once a second label chains on.
        assert_ne!(r2.labels_root, root_after_one);
        // The first label's chained hash is unchanged (its prefix is stable).
        assert_eq!(r2.labels[0].chained_hash, root_after_one);
        // Identity is preserved across appends.
        assert_eq!(r2.run_id, "run-1");
        assert_eq!(r2.session_id.as_deref(), Some("sess-1"));
    }

    #[test]
    fn summarize_tallies_runs_and_outcomes() {
        let mut merged_run = empty_record("a", None, None);
        merged_run = append_label(merged_run, label(0, Outcome::Merged, LabelSource::Human));
        merged_run = append_label(
            merged_run,
            label(1, Outcome::Incident, LabelSource::Observation),
        );

        let mut shipped_run = empty_record("b", None, None);
        shipped_run = append_label(
            shipped_run,
            label(0, Outcome::ShippedNoRegress, LabelSource::Ci),
        );

        let unlabeled = empty_record("c", None, None);

        let summary = summarize(&[merged_run, shipped_run, unlabeled]);
        assert_eq!(summary.total_runs, 3);
        assert_eq!(summary.labeled_runs, 2);
        assert_eq!(summary.merged, 1);
        assert_eq!(summary.incident, 1);
        assert_eq!(summary.shipped_no_regress, 1);
        assert_eq!(summary.reverted, 0);
        // Empty corpus is the all-zero default.
        assert_eq!(summarize(&[]), OutcomeSummary::default());
    }

    #[test]
    fn outcome_record_round_trips_through_json() {
        let mut record = empty_record("run-x", Some("s".into()), Some("gemini".into()));
        record = append_label(record, label(0, Outcome::Merged, LabelSource::Human));
        let json = serde_json::to_string(&record).expect("serialize");
        let back: OutcomeRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(record, back);
    }
}
