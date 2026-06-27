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
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

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

/// Advisory calibration over an outcome corpus — historical, observational signal a
/// human reads, **never** a gate (INV-R1: court reporter, not judge; INV-R3: advisory,
/// non-load-bearing). Integer-only (counts + parts-per-thousand rates, **no floats**), a
/// pure fold of its records, so it stays deterministic + golden-testable (INV-R2). This
/// is the shipped *deterministic* calibrator; a trained ML model (needing adoption data
/// and an inference runtime, hence out of the kernel) is the deferred upgrade behind the
/// same `OutcomeCalibrator` seam.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OutcomeCalibration {
    /// Records carrying at least one label (the population the rates are over).
    pub labeled_runs: u64,
    /// Labeled runs that shipped cleanly (a positive label, no negative one).
    pub shipped: u64,
    /// Labeled runs that regressed (any `reverted`/`incident` label).
    pub regressed: u64,
    /// Labeled runs carrying **both** a positive (`merged`/`shipped_no_regress`) and a
    /// negative (`reverted`/`incident`) label — the change cleared the bar but reality
    /// disagreed. Order-agnostic set membership (not a `seq`-ordered "then"); the
    /// corpus's weak-signal count, advisory only.
    pub passed_and_regressed: u64,
    /// Parts-per-thousand of labeled runs that shipped cleanly (`0` when none labeled).
    pub ship_permille: u64,
    /// Per-agent breakdown, agents in deterministic (sorted) order.
    pub by_agent: Vec<AgentCalibration>,
}

/// One agent's slice of the [`OutcomeCalibration`] — its labeled-run population and ship
/// rate, so a reader can compare agents. Advisory only.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentCalibration {
    /// The agent the slice is for (`"unattributed"` for records with no agent).
    pub agent: String,
    pub labeled_runs: u64,
    pub shipped: u64,
    pub regressed: u64,
    pub ship_permille: u64,
}

/// A running per-population tally of label classifications.
#[derive(Default, Clone, Copy)]
struct Tally {
    labeled: u64,
    shipped: u64,
    regressed: u64,
    passed_and_regressed: u64,
}

impl Tally {
    /// Fold one labeled run's `(has_positive, has_negative)` classification in.
    fn add(&mut self, (positive, negative): (bool, bool)) {
        self.labeled += 1;
        if negative {
            self.regressed += 1;
        }
        if positive && !negative {
            self.shipped += 1;
        }
        if positive && negative {
            self.passed_and_regressed += 1;
        }
    }
}

/// Classify a record's labels: did it accrue any positive (`merged`/`shipped_no_regress`)
/// and/or any negative (`reverted`/`incident`) outcome?
fn classify(record: &OutcomeRecord) -> (bool, bool) {
    let (mut positive, mut negative) = (false, false);
    for label in &record.labels {
        match label.outcome {
            Outcome::Merged | Outcome::ShippedNoRegress => positive = true,
            Outcome::Reverted | Outcome::Incident => negative = true,
        }
    }
    (positive, negative)
}

/// Parts-per-thousand of `num` out of `den` (integer; `0` when `den == 0`). Avoids
/// floats so the calibration stays `Eq`-derivable + byte-stable across platforms.
fn permille(num: u64, den: u64) -> u64 {
    num.saturating_mul(1000).checked_div(den).unwrap_or(0)
}

/// Deterministic per-check **flaky-rate** calibration (L6; INV-R1/R3) — the design's
/// headline "agent wrong vs. test flaky?" signal. A pure fold over a corpus of
/// [`Verdict`](nerve_proto::Verdict)s: for every `(check_name, kind)` pair, it counts
/// how many verdicts exercised it (`runs`), how many classed it
/// [`CheckStatus::Flaky`](nerve_proto::CheckStatus) (`flaky_runs`) or
/// [`CheckStatus::Fail`](nerve_proto::CheckStatus) (`fail_runs`), and the integer
/// `flaky_permille`. The output is **deterministically ordered** by `(check_name,
/// kind)` (no `HashMap` order leaks). No floats, no clock, no rng (INV-R2).
///
/// **Observational only — the result MUST NOT feed back into any verdict or gate**
/// (INV-R1/R3): a flaky-rate is *derived from* verdicts, it never alters one. It is a
/// signal a human reads to triage a noisy check.
#[must_use]
pub fn flaky_rates(verdicts: &[nerve_proto::Verdict]) -> Vec<nerve_proto::CheckFlakyRate> {
    use nerve_proto::{CheckFlakyRate, CheckKind, CheckStatus};
    // Tally by (name, kind); BTreeMap keeps the eventual Vec deterministically ordered.
    let mut tallies: BTreeMap<(String, CheckKindKey), (u64, u64, u64)> = BTreeMap::new();
    for verdict in verdicts {
        for check in &verdict.checks {
            let entry = tallies
                .entry((check.name.clone(), CheckKindKey::from(check.kind)))
                .or_insert((0, 0, 0));
            entry.0 += 1; // runs
            match check.status {
                CheckStatus::Flaky => entry.1 += 1,
                CheckStatus::Fail => entry.2 += 1,
                CheckStatus::Pass | CheckStatus::Error => {}
            }
        }
    }
    tallies
        .into_iter()
        .map(|((check_name, kind), (runs, flaky_runs, fail_runs))| {
            let kind: CheckKind = kind.into();
            CheckFlakyRate {
                check_name,
                kind,
                runs,
                flaky_runs,
                fail_runs,
                flaky_permille: permille(flaky_runs, runs),
            }
        })
        .collect()
}

/// An `Ord` proxy for [`CheckKind`](nerve_proto::CheckKind) (which is not `Ord`), so a
/// `(check_name, kind)` group key sorts deterministically. The numeric rank mirrors the
/// enum's declaration order and is internal-only (never serialized).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CheckKindKey(u8);

impl From<nerve_proto::CheckKind> for CheckKindKey {
    fn from(kind: nerve_proto::CheckKind) -> Self {
        use nerve_proto::CheckKind as K;
        Self(match kind {
            K::Test => 0,
            K::Typecheck => 1,
            K::Build => 2,
            K::Lint => 3,
            K::Property => 4,
            K::Mutation => 5,
            K::Contamination => 6,
        })
    }
}

impl From<CheckKindKey> for nerve_proto::CheckKind {
    fn from(key: CheckKindKey) -> Self {
        use nerve_proto::CheckKind as K;
        match key.0 {
            0 => K::Test,
            1 => K::Typecheck,
            2 => K::Build,
            3 => K::Lint,
            4 => K::Property,
            5 => K::Mutation,
            _ => K::Contamination,
        }
    }
}

/// Calibrate an outcome corpus into advisory [`OutcomeCalibration`] statistics — a pure,
/// deterministic fold (no IO, no wall-clock, no ML inference, no randomness; INV-R2).
/// Unlabeled records are skipped (no signal). Per-agent slices are emitted in sorted
/// agent order. **Observational only — the result MUST NOT feed back into any verdict
/// or gate** (INV-R1/R3); it answers "historically, how often did such runs ship?", for
/// a human to read, not Nerve to decide.
#[must_use]
pub fn calibrate(records: &[OutcomeRecord]) -> OutcomeCalibration {
    let mut overall = Tally::default();
    let mut per_agent: BTreeMap<String, Tally> = BTreeMap::new();
    for record in records {
        if record.labels.is_empty() {
            continue;
        }
        let class = classify(record);
        overall.add(class);
        let agent = record
            .agent
            .clone()
            .unwrap_or_else(|| "unattributed".to_string());
        per_agent.entry(agent).or_default().add(class);
    }
    OutcomeCalibration {
        labeled_runs: overall.labeled,
        shipped: overall.shipped,
        regressed: overall.regressed,
        passed_and_regressed: overall.passed_and_regressed,
        ship_permille: permille(overall.shipped, overall.labeled),
        by_agent: per_agent
            .into_iter()
            .map(|(agent, tally)| AgentCalibration {
                agent,
                labeled_runs: tally.labeled,
                shipped: tally.shipped,
                regressed: tally.regressed,
                ship_permille: permille(tally.shipped, tally.labeled),
            })
            .collect(),
    }
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
        let mut record = empty_record("run-x", Some("s".into()), Some("claude".into()));
        record = append_label(record, label(0, Outcome::Merged, LabelSource::Human));
        let json = serde_json::to_string(&record).expect("serialize");
        let back: OutcomeRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(record, back);
    }

    #[test]
    fn calibrate_is_a_deterministic_advisory_fold() {
        let rec = |run: &str, agent: Option<&str>, outcomes: &[Outcome]| {
            let mut record = empty_record(run, None, agent.map(str::to_string));
            for (seq, outcome) in outcomes.iter().enumerate() {
                record
                    .labels
                    .push(label(seq as u64, *outcome, LabelSource::Human));
            }
            record
        };
        let corpus = vec![
            rec("r1", Some("codex"), &[Outcome::Merged]), // shipped
            rec("r2", Some("codex"), &[Outcome::Merged, Outcome::Reverted]), // passed_then_regressed
            rec("r3", Some("claude"), &[Outcome::Incident]),                 // regressed
            rec("r4", Some("claude"), &[Outcome::ShippedNoRegress]),         // shipped
            rec("r5", Some("codex"), &[]),                                   // unlabeled -> skipped
        ];
        let cal = calibrate(&corpus);
        assert_eq!(cal.labeled_runs, 4);
        assert_eq!(cal.shipped, 2); // r1, r4
        assert_eq!(cal.regressed, 2); // r2, r3
        assert_eq!(cal.passed_and_regressed, 1); // r2 (both Merged and Reverted)
        assert_eq!(cal.ship_permille, 500); // 2/4

        // Per-agent slices in sorted agent order; unlabeled r5 doesn't inflate codex.
        assert_eq!(cal.by_agent.len(), 2);
        assert_eq!(cal.by_agent[0].agent, "claude");
        assert_eq!(cal.by_agent[0].labeled_runs, 2);
        assert_eq!(cal.by_agent[0].shipped, 1); // r4
        assert_eq!(cal.by_agent[1].agent, "codex");
        assert_eq!(cal.by_agent[1].labeled_runs, 2); // r1, r2
        assert_eq!(cal.by_agent[1].shipped, 1); // r1

        // Pure + deterministic: same corpus -> identical result.
        assert_eq!(cal, calibrate(&corpus));
        // Empty corpus -> zeroed, no agents, no div-by-zero panic.
        let empty = calibrate(&[]);
        assert_eq!(empty.labeled_runs, 0);
        assert_eq!(empty.ship_permille, 0);
        assert!(empty.by_agent.is_empty());
    }

    #[test]
    fn flaky_rates_is_a_deterministic_advisory_fold() {
        use nerve_proto::{CheckKind, CheckResult, CheckStatus, Verdict, VerdictStatus};

        let check = |name: &str, status: CheckStatus| CheckResult {
            name: name.into(),
            kind: CheckKind::Test,
            status,
            reproducible: false,
            exit_code: None,
            timed_out: false,
            duration_ms: 0,
            output_hash: String::new(),
            runs: 0,
            passed: 0,
        };
        let verdict = |checks: Vec<CheckResult>| Verdict {
            schema_version: 1,
            verdict_id: String::new(),
            run_id: "r".into(),
            diff_hash: None,
            status: VerdictStatus::Inconclusive,
            checkspec_hash: String::new(),
            closure_digest: String::new(),
            checks,
            verified_at_ms: 0,
            verdict_hash: String::new(),
        };

        // "always-flaky" is Flaky in all three verdicts; "stable" is always Pass.
        let corpus = vec![
            verdict(vec![
                check("always-flaky", CheckStatus::Flaky),
                check("stable", CheckStatus::Pass),
            ]),
            verdict(vec![
                check("always-flaky", CheckStatus::Flaky),
                check("stable", CheckStatus::Pass),
            ]),
            verdict(vec![
                check("always-flaky", CheckStatus::Flaky),
                check("stable", CheckStatus::Pass),
            ]),
        ];
        let rates = flaky_rates(&corpus);
        // Deterministically ordered by (check_name, kind).
        assert_eq!(rates.len(), 2);
        assert_eq!(rates[0].check_name, "always-flaky");
        assert_eq!(rates[0].runs, 3);
        assert_eq!(rates[0].flaky_runs, 3);
        assert_eq!(rates[0].fail_runs, 0);
        assert_eq!(rates[0].flaky_permille, 1000); // 3/3
        assert_eq!(rates[1].check_name, "stable");
        assert_eq!(rates[1].runs, 3);
        assert_eq!(rates[1].flaky_runs, 0);
        assert_eq!(rates[1].flaky_permille, 0);

        // A Fail is tallied into fail_runs, not flaky_runs.
        let mixed = vec![
            verdict(vec![check("x", CheckStatus::Flaky)]),
            verdict(vec![check("x", CheckStatus::Fail)]),
            verdict(vec![check("x", CheckStatus::Pass)]),
            verdict(vec![check("x", CheckStatus::Error)]),
        ];
        let mixed_rates = flaky_rates(&mixed);
        assert_eq!(mixed_rates.len(), 1);
        assert_eq!(mixed_rates[0].runs, 4);
        assert_eq!(mixed_rates[0].flaky_runs, 1);
        assert_eq!(mixed_rates[0].fail_runs, 1);
        assert_eq!(mixed_rates[0].flaky_permille, 250); // 1/4

        // Pure + deterministic: same corpus -> identical result; empty -> empty.
        assert_eq!(flaky_rates(&corpus), flaky_rates(&corpus));
        assert!(flaky_rates(&[]).is_empty());
    }
}
