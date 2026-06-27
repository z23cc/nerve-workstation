//! Durable L6 **outcome-corpus** persistence (`docs/designs/trust-substrate.md`
//! §3 L6, §8) — the sibling of [`RunStore`](crate::run_store) and
//! [`DelegateStore`](crate::delegate_store). After a run is captured (L0), gated by
//! a verdict (L2) and a signed receipt (L4), the org records what *actually
//! happened* to the change — was it `merged`, later `reverted`, implicated in an
//! `incident`, or `shipped_no_regress` — as an append-only, tamper-evident
//! [`OutcomeRecord`] tape keyed by `run_id`.
//!
//! ```text
//! .nerve/outcomes/<run_id>.json   # the versioned OutcomeRecord for one run
//! ```
//!
//! Mirrors the verified [`RunStore`] discipline — a versioned record (a
//! `schema_version`, a tolerant [`load_record`](OutcomeStore::load_record) path,
//! and a [`migrate_to_current`] seam owned by THIS module), atomic writes (temp +
//! rename), and **best-effort** persistence: a write failure NEVER fails the
//! delegated turn (the corpus is an advisory audit seam, not a gate — INV-R1). The
//! pure chaining/hashing they call lives in `nerve-core::outcome` (INV-R2): this
//! file never hashes, it loads-or-creates, appends via the pure helper, and
//! persists.
//!
//! Labels are **non-load-bearing** observations: they are never an input to a
//! verdict (INV-R1/R3/R4). The deferred [`OutcomeCalibrator`] seam (a future ML
//! model reading this corpus) ships today as [`NoCalibrator`] — it returns `None`,
//! so calibration is advisory-by-construction.

use anyhow::{Context, Result, anyhow};
use nerve_core::outcome::{
    LabelSource, OUTCOME_SCHEMA_VERSION, Outcome, OutcomeLabel, OutcomeRecord,
};
use nerve_runtime::RuntimeError;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Calibration seam (`trust-substrate.md` §5, INV-R1/R3/R4): a model that reads the
/// outcome corpus to *advise* (ship rate, per-agent rates, the passed-then-regressed
/// weak signal) — never to judge. The result is **observational only**: it MUST NOT feed
/// back into any verdict or gate. The shipped impl is the deterministic
/// [`CorpusCalibrator`]; a trained ML model (needing adoption data + an inference runtime,
/// hence above the kernel's determinism boundary) is the deferred upgrade behind this
/// same trait — mirroring the L4 `Signer` seam (local-ed25519 shipped, sigstore deferred).
pub(crate) trait OutcomeCalibrator {
    /// Advise over a corpus of records, returning `Some(advisory_json)` when there is
    /// signal (≥1 labeled run), else `None`. Observational only — never a verdict.
    fn calibrate(&self, records: &[OutcomeRecord]) -> Option<Value>;
}

/// The shipped calibrator: the pure, deterministic [`nerve_core::outcome::calibrate`]
/// fold (counts + integer ship rates, no ML, no floats). Returns `None` on an unlabeled
/// corpus (no signal yet — honest), else the advisory `OutcomeCalibration` as JSON.
pub(crate) struct CorpusCalibrator;

impl OutcomeCalibrator for CorpusCalibrator {
    fn calibrate(&self, records: &[OutcomeRecord]) -> Option<Value> {
        let calibration = nerve_core::outcome::calibrate(records);
        if calibration.labeled_runs == 0 {
            return None;
        }
        serde_json::to_value(&calibration).ok()
    }
}

/// A directory of persisted outcome corpora (`<dir>/<run_id>.json`). Sibling of
/// [`RunStore`](crate::run_store).
#[derive(Clone)]
pub(crate) struct OutcomeStore {
    dir: PathBuf,
}

impl OutcomeStore {
    /// Wrap an explicit outcomes directory.
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Resolve the outcomes directory for a scope: `<root>/.nerve/outcomes` for a
    /// project root, else the global `config_home()/outcomes`.
    pub(crate) fn for_scope(root: Option<&Path>) -> Result<Self> {
        Ok(Self::new(resolve_outcomes_dir(root)?))
    }

    /// The backing directory (mirrors `RunStore::dir`; used by tests).
    #[allow(dead_code, reason = "accessor mirroring RunStore::dir; used by tests")]
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// The per-run file `<dir>/<run_id>.json` (validating the id stays in-dir).
    fn path_for(&self, run_id: &str) -> Result<PathBuf> {
        validate_id(run_id)?;
        Ok(self.dir.join(format!("{run_id}.json")))
    }

    /// Persist an outcome record atomically (temp + rename), creating the dir on
    /// demand.
    pub(crate) fn write_record(&self, record: &OutcomeRecord) -> Result<()> {
        let path = self.path_for(&record.run_id)?;
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("failed to create outcomes dir {}", self.dir.display()))?;
        let json = serde_json::to_string_pretty(record).context("serialize outcome record")?;
        atomic_write(&path, json.as_bytes())
    }

    /// Load and migrate one outcome record by run id.
    pub(crate) fn load_record(&self, run_id: &str) -> Result<OutcomeRecord> {
        let path = self.path_for(run_id)?;
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        deserialize_record(&raw).with_context(|| format!("failed to parse outcome `{run_id}`"))
    }

    /// All persisted outcome records (tolerating a missing dir + bad files),
    /// ordered by `run_id` for a stable enumeration.
    pub(crate) fn list(&self) -> Result<Vec<OutcomeRecord>> {
        let mut records = Vec::new();
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(records),
            Err(err) => return Err(anyhow!("failed to read {}: {err}", self.dir.display())),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(raw) = fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(record) = deserialize_record(&raw) {
                records.push(record);
            }
        }
        records.sort_by(|a, b| a.run_id.cmp(&b.run_id));
        Ok(records)
    }
}

/// Append one outcome label to a run's corpus (`outcome.label`). Loads the existing
/// record (or starts an [`empty_record`](nerve_core::outcome::empty_record) for the
/// run), appends the label via the pure chaining helper, persists, and returns the
/// announce payload `{ "record": <OutcomeRecord> }` plus the fields the integrator
/// needs to emit [`RuntimeEvent::outcome_labeled`](nerve_runtime::RuntimeEvent).
///
/// `observed_at_ms` is host wall-clock (supplied here, never hashed). A persistence
/// failure surfaces as an adapter error so the caller can decide; an unknown
/// `run_id` is fine — the corpus is created on first label.
///
/// Returns the announce payload, the `run_id`, the refreshed `labels_root`, the label
/// count, and the just-appended label's content `label_hash` — the last so the caller
/// can best-effort mirror this observation onto the L1 evidence ledger as a
/// [`LedgerKind::OutcomeRecorded`](nerve_core::ledger::LedgerKind) (an observation,
/// never a verdict input — INV-R1/R3/R4).
#[derive(Debug)]
pub(crate) struct LabeledOutcome {
    pub(crate) payload: Value,
    pub(crate) run_id: String,
    pub(crate) labels_root: String,
    pub(crate) label_count: u64,
    pub(crate) label_hash: String,
}

pub(crate) fn handle_outcome_label(
    run_id: &str,
    outcome: Outcome,
    source: LabelSource,
    actor: Option<String>,
    note: Option<String>,
    verdict_ref: Option<String>,
    store: Option<&OutcomeStore>,
) -> Result<LabeledOutcome, RuntimeError> {
    let store = store
        .ok_or_else(|| RuntimeError::adapter(format!("no served root for outcome `{run_id}`")))?;
    // Load the existing corpus (preserving its denormalized identity), or start a
    // fresh empty record for this run — the corpus is created on first label.
    let record = store
        .load_record(run_id)
        .unwrap_or_else(|_| nerve_core::outcome::empty_record(run_id, None, None));
    let next_seq = record.labels.len() as u64;
    let label = OutcomeLabel {
        seq: next_seq,
        outcome,
        source,
        actor,
        note,
        verdict_ref,
        observed_at_ms: now_ms(),
        label_hash: String::new(),
        chained_hash: String::new(),
    };
    let record = nerve_core::outcome::append_label(record, label);
    store.write_record(&record).map_err(|err| {
        RuntimeError::adapter(format!("failed to persist outcome `{run_id}`: {err}"))
    })?;
    let labels_root = record.labels_root.clone();
    let label_count = record.labels.len() as u64;
    // The just-appended label is the chain tail; its content hash is the L1 anchor.
    let label_hash = record
        .labels
        .last()
        .map(|label| label.label_hash.clone())
        .unwrap_or_default();
    let payload = serde_json::to_value(&record)
        .map(|record| json!({ "record": record }))
        .map_err(|err| {
            RuntimeError::adapter(format!("failed to render outcome `{run_id}`: {err}"))
        })?;
    Ok(LabeledOutcome {
        payload,
        run_id: run_id.to_string(),
        labels_root,
        label_count,
        label_hash,
    })
}

/// Resolve an `outcome.get`: the full [`OutcomeRecord`] by run id. An unknown id (or
/// no served root) is an error, mirroring `run.get`.
pub(crate) fn handle_outcome_get(
    run_id: &str,
    store: Option<&OutcomeStore>,
) -> Result<Value, RuntimeError> {
    let store =
        store.ok_or_else(|| RuntimeError::adapter(format!("no outcome corpus for `{run_id}`")))?;
    let record = store
        .load_record(run_id)
        .map_err(|err| RuntimeError::adapter(format!("no outcome corpus for `{run_id}`: {err}")))?;
    let record = serde_json::to_value(&record).map_err(|err| {
        RuntimeError::adapter(format!("failed to render outcome `{run_id}`: {err}"))
    })?;
    Ok(json!({ "record": record }))
}

/// Resolve an `outcome.query`: the matching records (filtered by `agent` and/or by
/// the presence of an `outcome` disposition, capped at `limit`) plus a rolled-up
/// [`OutcomeSummary`](nerve_core::outcome::OutcomeSummary), the advisory `calibration`
/// (the [`CorpusCalibrator`] over the *matched* set — `null` when no run is labeled),
/// and the advisory `flaky_rates` (the deterministic per-check flaky-rate fold over the
/// served verdict corpus). `None` store (no served root) yields an empty result. Every
/// rollup is a pure fold ([`summarize`](nerve_core::outcome::summarize) /
/// [`calibrate`](nerve_core::outcome::calibrate) /
/// [`flaky_rates`](nerve_core::outcome::flaky_rates)); they are **observational only**
/// (INV-R1/R3), never a gate. The verdict corpus is read best-effort — a
/// [`VerifyStore`] read failure degrades `flaky_rates` to an empty list, never an error.
pub(crate) fn handle_outcome_query(
    agent: Option<&str>,
    outcome: Option<Outcome>,
    limit: u64,
    store: Option<&OutcomeStore>,
    verify_store: Option<&crate::verify_store::VerifyStore>,
) -> Value {
    let all = store.and_then(|s| s.list().ok()).unwrap_or_default();
    let matched: Vec<OutcomeRecord> = all
        .into_iter()
        .filter(|record| agent.is_none_or(|a| record.agent.as_deref() == Some(a)))
        .filter(|record| {
            outcome.is_none_or(|want| record.labels.iter().any(|label| label.outcome == want))
        })
        .take(limit as usize)
        .collect();
    let summary = nerve_core::outcome::summarize(&matched);
    let records = matched
        .iter()
        .map(|record| serde_json::to_value(record).unwrap_or(Value::Null))
        .collect::<Vec<_>>();
    let summary = serde_json::to_value(&summary).unwrap_or(Value::Null);
    // Advisory only (INV-R1/R3): surfaced for a human to read, never fed to a verdict.
    let calibration = CorpusCalibrator.calibrate(&matched);
    // Best-effort: a verdict-corpus read failure degrades to an empty flaky-rate vec.
    let verdicts = verify_store.and_then(|s| s.list().ok()).unwrap_or_default();
    let flaky_rates = serde_json::to_value(nerve_core::outcome::flaky_rates(&verdicts))
        .unwrap_or_else(|_| Value::Array(Vec::new()));
    json!({
        "records": records,
        "summary": summary,
        "calibration": calibration,
        "flaky_rates": flaky_rates,
    })
}

/// Atomic file write: temp file + rename, so a reader never observes a half-written
/// file. `rename` is atomic within a directory on the platforms we target.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("path {} has no parent", path.display()))?;
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("outcome-write")
    ));
    fs::write(&tmp, bytes).with_context(|| format!("failed to write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("failed to rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Parse + migrate an outcome record, tolerant of an older/missing `schema_version`
/// (treated as v1); rejects a newer-than-supported version.
fn deserialize_record(raw: &str) -> Result<OutcomeRecord> {
    let mut value: Value = serde_json::from_str(raw).context("invalid outcome JSON")?;
    let version = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    migrate_to_current(&mut value, version)?;
    serde_json::from_value(value).context("outcome shape mismatch")
}

/// Upgrade an outcome `value` from `version` to [`OUTCOME_SCHEMA_VERSION`] in place.
/// Only one version exists today, so this is the newer-than-known guard + a
/// re-stamp; add an arm per future bump (mirrors `RunStore`).
fn migrate_to_current(value: &mut Value, version: u64) -> Result<()> {
    if version > u64::from(OUTCOME_SCHEMA_VERSION) {
        return Err(anyhow!(
            "outcome schema_version {version} is newer than supported {OUTCOME_SCHEMA_VERSION}; upgrade nerve"
        ));
    }
    if let Some(object) = value.as_object_mut() {
        object.insert("schema_version".into(), Value::from(OUTCOME_SCHEMA_VERSION));
    }
    Ok(())
}

/// `<root>/.nerve/outcomes` for a project root, else the global
/// `config_home()/outcomes`.
fn resolve_outcomes_dir(root: Option<&Path>) -> Result<PathBuf> {
    match root {
        Some(root) => Ok(root.join(".nerve").join("outcomes")),
        None => {
            let home = nerve_agent::auth::config_home().map_err(|err| anyhow!("{err}"))?;
            Ok(home.join("outcomes"))
        }
    }
}

/// Reject ids that could escape the outcomes directory (same token rule as the
/// other stores: ASCII alphanumerics plus `-`/`_`). A content-address run id is hex,
/// so it always passes; this guards against a malformed/empty id reaching the
/// filesystem.
fn validate_id(id: &str) -> Result<()> {
    let valid = !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    if valid {
        Ok(())
    } else {
        Err(anyhow!(
            "invalid run id '{id}': use only letters, digits, '-' and '_'"
        ))
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn for_scope_uses_project_nerve_outcomes() {
        let store = OutcomeStore::for_scope(Some(Path::new("/tmp/proj"))).unwrap();
        assert_eq!(store.dir(), Path::new("/tmp/proj/.nerve/outcomes"));
    }

    #[test]
    fn outcome_query_surfaces_advisory_calibration() {
        let dir = tempdir().unwrap();
        let store = OutcomeStore::new(dir.path().join("outcomes"));
        // One clean ship + one merged-then-reverted (passed the bar, then regressed).
        let label = |run: &str, outcome: Outcome, source: LabelSource| {
            handle_outcome_label(run, outcome, source, None, None, None, Some(&store)).unwrap();
        };
        label("run-ship", Outcome::Merged, LabelSource::Human);
        label("run-regress", Outcome::Merged, LabelSource::Human);
        label("run-regress", Outcome::Reverted, LabelSource::Ci);

        // The query surfaces the advisory calibration alongside the summary.
        let value = handle_outcome_query(None, None, 100, Some(&store), None);
        let cal = &value["calibration"];
        assert_eq!(cal["labeled_runs"], json!(2));
        assert_eq!(cal["shipped"], json!(1)); // run-ship
        assert_eq!(cal["regressed"], json!(1)); // run-regress
        assert_eq!(cal["passed_and_regressed"], json!(1)); // run-regress (Merged + Reverted)
        assert_eq!(cal["ship_permille"], json!(500)); // 1/2

        // No signal => the calibrator declines and the query renders calibration: null
        // (never fabricates an advisory). Verified both directly and end-to-end.
        assert!(CorpusCalibrator.calibrate(&[]).is_none());
        let unlabeled = nerve_core::outcome::empty_record("u", None, None);
        assert!(CorpusCalibrator.calibrate(&[unlabeled]).is_none());
        let empty_dir = tempdir().unwrap();
        let empty_store = OutcomeStore::new(empty_dir.path().join("outcomes"));
        let empty_value = handle_outcome_query(None, None, 100, Some(&empty_store), None);
        assert!(empty_value["calibration"].is_null());
    }

    #[test]
    fn outcome_query_attaches_flaky_rates_from_the_verdict_corpus() {
        use nerve_core::verdict::{CheckKind, CheckResult, CheckStatus, Verdict, VerdictStatus};

        let dir = tempdir().unwrap();
        let store = OutcomeStore::new(dir.path().join("outcomes"));
        let verify_dir = tempdir().unwrap();
        let verify_store =
            crate::verify_store::VerifyStore::new(verify_dir.path().join("verdicts"));

        // Seed a verdict corpus: a "flaky-check" that is Flaky in both verdicts and a
        // "stable" check that always passes. verdict_id must be unique per file.
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
        for (i, flaky) in [CheckStatus::Flaky, CheckStatus::Flaky]
            .into_iter()
            .enumerate()
        {
            let verdict = Verdict {
                schema_version: 1,
                verdict_id: format!("v-{i}"),
                run_id: "r".into(),
                diff_hash: None,
                status: VerdictStatus::Inconclusive,
                checkspec_hash: String::new(),
                closure_digest: String::new(),
                checks: vec![
                    check("flaky-check", flaky),
                    check("stable", CheckStatus::Pass),
                ],
                verified_at_ms: i as u64,
                verdict_hash: format!("h-{i}"),
            };
            verify_store.write_record(&verdict).unwrap();
        }

        // One label so the query returns; flaky_rates is computed independently from
        // the verdict corpus (advisory, INV-R1/R3 — never feeds the label/verdict).
        handle_outcome_label(
            "r",
            Outcome::Merged,
            LabelSource::Human,
            None,
            None,
            None,
            Some(&store),
        )
        .unwrap();

        let value = handle_outcome_query(None, None, 100, Some(&store), Some(&verify_store));
        let rates = value["flaky_rates"].as_array().expect("flaky_rates array");
        // Deterministically ordered by (check_name, kind): flaky-check, then stable.
        assert_eq!(rates.len(), 2);
        assert_eq!(rates[0]["check_name"], json!("flaky-check"));
        assert_eq!(rates[0]["runs"], json!(2));
        assert_eq!(rates[0]["flaky_runs"], json!(2));
        assert_eq!(rates[0]["flaky_permille"], json!(1000));
        assert_eq!(rates[1]["check_name"], json!("stable"));
        assert_eq!(rates[1]["flaky_runs"], json!(0));
        assert_eq!(rates[1]["flaky_permille"], json!(0));

        // No verify store (or an empty corpus) -> an empty (best-effort) flaky_rates.
        let none_vs = handle_outcome_query(None, None, 100, Some(&store), None);
        assert_eq!(none_vs["flaky_rates"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn label_creates_corpus_chains_and_round_trips() {
        let dir = tempdir().unwrap();
        let store = OutcomeStore::new(dir.path().join("outcomes"));

        // First label creates the corpus.
        let first = handle_outcome_label(
            "run-1",
            Outcome::Merged,
            LabelSource::Human,
            Some("alice".into()),
            Some("looks good".into()),
            None,
            Some(&store),
        )
        .unwrap();
        let root1 = first.labels_root.clone();
        assert_eq!(first.run_id, "run-1");
        assert_eq!(first.label_count, 1);
        assert!(!root1.is_empty());
        // The returned label_hash is the just-appended label's content digest.
        assert_eq!(
            first.payload["record"]["labels"][0]["label_hash"],
            json!(first.label_hash)
        );
        assert!(!first.label_hash.is_empty());
        assert_eq!(first.payload["record"]["run_id"], json!("run-1"));
        assert_eq!(
            first.payload["record"]["labels"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            first.payload["record"]["labels"][0]["actor"],
            json!("alice")
        );
        assert_eq!(first.payload["record"]["labels_root"], json!(root1));

        // Second label appends, advancing the chain head and seq.
        let second = handle_outcome_label(
            "run-1",
            Outcome::Reverted,
            LabelSource::Ci,
            None,
            None,
            Some("v-7".into()),
            Some(&store),
        )
        .unwrap();
        assert_eq!(second.label_count, 2);
        assert_ne!(second.labels_root, root1);
        assert_eq!(second.payload["record"]["labels"][1]["seq"], json!(1));
        assert_eq!(
            second.payload["record"]["labels"][1]["verdict_ref"],
            json!("v-7")
        );

        // The persisted record matches a pure rebuild over the same labels (the
        // store does not perturb the chain) — no hardcoded hex.
        let loaded = store.load_record("run-1").unwrap();
        assert_eq!(loaded.labels.len(), 2);
        let rebuilt = nerve_core::outcome::seal_labels(loaded.labels.clone()).1;
        assert_eq!(rebuilt, loaded.labels_root);
    }

    #[test]
    fn label_without_store_errors() {
        let err = handle_outcome_label(
            "r",
            Outcome::Merged,
            LabelSource::Human,
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no served root"), "{err}");
    }

    #[test]
    fn get_returns_record_or_errors() {
        let dir = tempdir().unwrap();
        let store = OutcomeStore::new(dir.path().join("outcomes"));
        handle_outcome_label(
            "run-7",
            Outcome::ShippedNoRegress,
            LabelSource::Observation,
            None,
            None,
            None,
            Some(&store),
        )
        .unwrap();

        let got = handle_outcome_get("run-7", Some(&store)).unwrap();
        assert_eq!(got["record"]["run_id"], json!("run-7"));
        assert_eq!(got["record"]["labels"].as_array().unwrap().len(), 1);

        assert!(handle_outcome_get("missing", Some(&store)).is_err());
        assert!(handle_outcome_get("x", None).is_err());
    }

    #[test]
    fn query_filters_by_agent_and_outcome_and_summarizes() {
        let dir = tempdir().unwrap();
        let store = OutcomeStore::new(dir.path().join("outcomes"));

        // Two corpora with denormalized agents (write records directly to set agent).
        let mut codex = nerve_core::outcome::empty_record("run-a", None, Some("codex".into()));
        codex = nerve_core::outcome::append_label(
            codex,
            OutcomeLabel {
                seq: 0,
                outcome: Outcome::Merged,
                source: LabelSource::Human,
                actor: None,
                note: None,
                verdict_ref: None,
                observed_at_ms: 1,
                label_hash: String::new(),
                chained_hash: String::new(),
            },
        );
        store.write_record(&codex).unwrap();

        let mut claude = nerve_core::outcome::empty_record("run-b", None, Some("claude".into()));
        claude = nerve_core::outcome::append_label(
            claude,
            OutcomeLabel {
                seq: 0,
                outcome: Outcome::Reverted,
                source: LabelSource::Ci,
                actor: None,
                note: None,
                verdict_ref: None,
                observed_at_ms: 2,
                label_hash: String::new(),
                chained_hash: String::new(),
            },
        );
        store.write_record(&claude).unwrap();

        // Unfiltered: both, summary tallies both dispositions.
        let all = handle_outcome_query(None, None, 100, Some(&store), None);
        assert_eq!(all["records"].as_array().unwrap().len(), 2);
        assert_eq!(all["summary"]["total_runs"], json!(2));
        assert_eq!(all["summary"]["merged"], json!(1));
        assert_eq!(all["summary"]["reverted"], json!(1));

        // Filter by agent.
        let only_codex = handle_outcome_query(Some("codex"), None, 100, Some(&store), None);
        assert_eq!(only_codex["records"].as_array().unwrap().len(), 1);
        assert_eq!(only_codex["records"][0]["run_id"], json!("run-a"));

        // Filter by outcome disposition.
        let only_reverted =
            handle_outcome_query(None, Some(Outcome::Reverted), 100, Some(&store), None);
        assert_eq!(only_reverted["records"].as_array().unwrap().len(), 1);
        assert_eq!(only_reverted["records"][0]["run_id"], json!("run-b"));

        // Limit caps the result set.
        let capped = handle_outcome_query(None, None, 1, Some(&store), None);
        assert_eq!(capped["records"].as_array().unwrap().len(), 1);

        // None store -> empty.
        let empty = handle_outcome_query(None, None, 100, None, None);
        assert_eq!(empty["records"].as_array().unwrap().len(), 0);
        assert_eq!(empty["summary"]["total_runs"], json!(0));
    }

    #[test]
    fn newer_schema_version_is_rejected() {
        let raw = json!({ "schema_version": 999, "run_id": "r" }).to_string();
        let err = deserialize_record(&raw).unwrap_err();
        assert!(err.to_string().contains("newer than supported"), "{err}");
    }

    #[test]
    fn invalid_ids_are_rejected_on_write() {
        let dir = tempdir().unwrap();
        let store = OutcomeStore::new(dir.path().to_path_buf());
        for bad in ["../escape", "a/b", "", "dots.here"] {
            let mut record = nerve_core::outcome::empty_record("ok", None, None);
            record.run_id = bad.to_string();
            assert!(
                store.write_record(&record).is_err(),
                "expected '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn list_tolerates_missing_dir_and_orders_by_run_id() {
        let dir = tempdir().unwrap();
        let store = OutcomeStore::new(dir.path().join("outcomes"));
        assert!(store.list().unwrap().is_empty(), "missing dir is empty");

        for run in ["c", "a", "b"] {
            store
                .write_record(&nerve_core::outcome::empty_record(run, None, None))
                .unwrap();
        }
        let ids: Vec<String> = store
            .list()
            .unwrap()
            .into_iter()
            .map(|r| r.run_id)
            .collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }
}
