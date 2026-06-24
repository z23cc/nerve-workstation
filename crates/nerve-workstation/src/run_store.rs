//! Durable L0 run-capture persistence (`docs/designs/trust-substrate.md` §3 L0/L1,
//! §6) — the sibling of [`DelegateStore`](crate::delegate_store). Every delegated
//! run is captured as an ordered, content-addressed [`Run`] (event tape + hash-chain
//! ledger, sealed by [`nerve_core::build_run`]) and persisted so it can be
//! enumerated (`run.list`), fetched (`run.get`), and — in a later brick — replayed
//! and re-verified into a portable Receipt.
//!
//! ```text
//! .nerve/runs/<run_id>.json   # the versioned Run (run_id == its content address)
//! ```
//!
//! Mirrors the verified [`DelegateStore`] discipline — a versioned record (a
//! `schema_version`, a tolerant [`load_record`](RunStore::load_record) path, and a
//! [`migrate_to_current`] seam owned by THIS module), atomic writes (temp + rename),
//! and **best-effort** capture: a persistence failure NEVER fails the delegated turn
//! (provenance is an audit seam, not a gate). Capture and IO live here in
//! `nerve-workstation`, above the determinism boundary; the pure canonicalization and
//! hashing they call live in `nerve-core::provenance` (INV-R2).

use anyhow::{Context, Result, anyhow};
use nerve_core::provenance::{Event, EventKind, RUN_SCHEMA_VERSION, Run};
use nerve_runtime::RuntimeError;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A directory of persisted captured runs (`<dir>/<run_id>.json`). Sibling of
/// [`DelegateStore`](crate::delegate_store).
#[derive(Clone)]
pub(crate) struct RunStore {
    dir: PathBuf,
}

impl RunStore {
    /// Wrap an explicit runs directory.
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Resolve the runs directory for a scope: `<root>/.nerve/runs` for a project
    /// root, else the global `config_home()/runs`.
    pub(crate) fn for_scope(root: Option<&Path>) -> Result<Self> {
        Ok(Self::new(resolve_runs_dir(root)?))
    }

    /// The backing directory (mirrors `DelegateStore::dir`; used by tests).
    #[allow(
        dead_code,
        reason = "accessor mirroring DelegateStore::dir; used by tests"
    )]
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// The per-run file `<dir>/<run_id>.json` (validating the id stays in-dir).
    fn path_for(&self, run_id: &str) -> Result<PathBuf> {
        validate_id(run_id)?;
        Ok(self.dir.join(format!("{run_id}.json")))
    }

    /// Persist a run atomically (temp + rename), creating the dir on demand.
    pub(crate) fn write_record(&self, run: &Run) -> Result<()> {
        let path = self.path_for(&run.run_id)?;
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("failed to create runs dir {}", self.dir.display()))?;
        let json = serde_json::to_string_pretty(run).context("serialize run")?;
        atomic_write(&path, json.as_bytes())
    }

    /// Load and migrate one run by id.
    pub(crate) fn load_record(&self, run_id: &str) -> Result<Run> {
        let path = self.path_for(run_id)?;
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        deserialize_record(&raw).with_context(|| format!("failed to parse run {run_id}"))
    }

    /// All persisted runs, most recent first (tolerating a missing dir + bad files).
    pub(crate) fn list(&self) -> Result<Vec<Run>> {
        let mut runs = Vec::new();
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(runs),
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
            if let Ok(run) = deserialize_record(&raw) {
                runs.push(run);
            }
        }
        runs.sort_by(|a, b| {
            b.started_at_ms
                .cmp(&a.started_at_ms)
                .then_with(|| b.run_id.cmp(&a.run_id))
        });
        Ok(runs)
    }
}

/// In-memory accumulator for one delegated run's tape. Captures events with a
/// monotonic logical `seq` (the deterministic clock that feeds the hash — never
/// wall-clock), then [`seal`](RunWriter::seal)s them into a content-addressed
/// [`Run`] and persists it. Best-effort throughout: a missing store or a write
/// failure yields `None` and never propagates into the delegated turn.
pub(crate) struct RunWriter {
    session_id: String,
    agent: String,
    root: Option<String>,
    started_at_ms: u64,
    seq: u64,
    events: Vec<Event>,
}

/// The identity of a sealed, persisted run — returned by [`RunWriter::seal`] so the
/// host can announce it via [`RuntimeEvent::run_recorded`](nerve_runtime::RuntimeEvent).
pub(crate) struct SealedRun {
    pub(crate) run_id: String,
    pub(crate) root_hash: String,
    pub(crate) event_count: u64,
}

impl RunWriter {
    /// Begin capturing a run for the given delegated session (`session_id` is the
    /// `delegate.start` job id) against `agent`, confined to `root`, stamping the
    /// start time as now (the one-shot path, where capture begins at the spawn).
    pub(crate) fn begin(
        session_id: impl Into<String>,
        agent: impl Into<String>,
        root: Option<String>,
    ) -> Self {
        Self::begin_at(now_ms(), session_id, agent, root)
    }

    /// Begin with an explicit start timestamp — for the live-session path, where the
    /// tape is assembled at close but `started_at_ms` must reflect when turn 1 began.
    /// `started_at_ms` is display metadata only and is never hashed (INV-R2), so this
    /// never affects the content address.
    pub(crate) fn begin_at(
        started_at_ms: u64,
        session_id: impl Into<String>,
        agent: impl Into<String>,
        root: Option<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            agent: agent.into(),
            root,
            started_at_ms,
            seq: 0,
            events: Vec::new(),
        }
    }

    /// Append one typed event, assigning the next monotonic logical `seq`.
    pub(crate) fn push(&mut self, kind: EventKind) {
        self.events.push(Event {
            seq: self.seq,
            kind,
        });
        self.seq += 1;
    }

    /// Seal the tape into a content-addressed [`Run`] and persist it to `store`.
    /// Returns the sealed run's identity on success, or `None` when there is no
    /// store or the write failed (best-effort — the caller continues regardless).
    pub(crate) fn seal(self, finished: bool, store: Option<&RunStore>) -> Option<SealedRun> {
        let event_count = self.events.len() as u64;
        let run = nerve_core::build_run(
            self.session_id,
            self.agent,
            self.root,
            self.started_at_ms,
            Some(now_ms()),
            finished,
            self.events,
        );
        let store = store?;
        match store.write_record(&run) {
            Ok(()) => Some(SealedRun {
                run_id: run.run_id,
                root_hash: run.root_hash,
                event_count,
            }),
            Err(_) => None,
        }
    }
}

/// Resolve a `run.list`: all captured runs for the served scope, newest first.
/// `None` store (no served root) yields an empty list.
pub(crate) fn run_run_list(store: Option<&RunStore>) -> Value {
    let runs = store
        .and_then(|s| s.list().ok())
        .unwrap_or_default()
        .iter()
        .map(|run| serde_json::to_value(run).unwrap_or(Value::Null))
        .collect::<Vec<_>>();
    json!({ "runs": runs })
}

/// Resolve a `run.get`: the full captured [`Run`] by id. An unknown id (or no
/// served root) is an error, mirroring `delegate.get`.
pub(crate) fn run_run_get(run_id: &str, store: Option<&RunStore>) -> Result<Value, RuntimeError> {
    let store =
        store.ok_or_else(|| RuntimeError::adapter(format!("no captured run `{run_id}`")))?;
    let run = store
        .load_record(run_id)
        .map_err(|err| RuntimeError::adapter(format!("no captured run `{run_id}`: {err}")))?;
    let run = serde_json::to_value(&run)
        .map_err(|err| RuntimeError::adapter(format!("failed to render run `{run_id}`: {err}")))?;
    Ok(json!({ "run": run }))
}

/// Convert a reported USD cost to integer micro-USD for the hashed `UsageUpdated`
/// event (no floats in the digest — INV-R2). Negative/NaN costs map to `None`.
pub(crate) fn cost_to_micro_usd(cost_usd: Option<f64>) -> Option<u64> {
    cost_usd.and_then(|usd| {
        if usd.is_finite() && usd >= 0.0 {
            Some((usd * 1_000_000.0).round() as u64)
        } else {
            None
        }
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
            .unwrap_or("run-write")
    ));
    fs::write(&tmp, bytes).with_context(|| format!("failed to write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("failed to rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Parse + migrate a run, tolerant of an older/missing `schema_version` (treated as
/// v1); rejects a newer-than-supported version.
fn deserialize_record(raw: &str) -> Result<Run> {
    let mut value: Value = serde_json::from_str(raw).context("invalid run JSON")?;
    let version = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    migrate_to_current(&mut value, version)?;
    serde_json::from_value(value).context("run shape mismatch")
}

/// Upgrade a run `value` from `version` to [`RUN_SCHEMA_VERSION`] in place. Only one
/// version exists today, so this is the newer-than-known guard + a re-stamp; add an
/// arm per future bump (mirrors `DelegateStore` / `FlowStore`).
fn migrate_to_current(value: &mut Value, version: u64) -> Result<()> {
    if version > u64::from(RUN_SCHEMA_VERSION) {
        return Err(anyhow!(
            "run schema_version {version} is newer than supported {RUN_SCHEMA_VERSION}; upgrade nerve"
        ));
    }
    if let Some(object) = value.as_object_mut() {
        object.insert("schema_version".into(), Value::from(RUN_SCHEMA_VERSION));
    }
    Ok(())
}

/// `<root>/.nerve/runs` for a project root, else the global `config_home()/runs`.
fn resolve_runs_dir(root: Option<&Path>) -> Result<PathBuf> {
    match root {
        Some(root) => Ok(root.join(".nerve").join("runs")),
        None => {
            let home = nerve_agent::auth::config_home().map_err(|err| anyhow!("{err}"))?;
            Ok(home.join("runs"))
        }
    }
}

/// Reject ids that could escape the runs directory (same token rule as the other
/// stores: ASCII alphanumerics plus `-`/`_`). A content-address run id is hex, so it
/// always passes; this guards against a malformed/empty id reaching the filesystem.
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

    fn sample_writer(store: &RunStore) -> SealedRun {
        let mut writer = RunWriter::begin("job-7", "codex", Some("/repo".into()));
        writer.push(EventKind::RunStarted {
            agent: "codex".into(),
            task: "add a test".into(),
            cwd: Some("/repo".into()),
        });
        writer.push(EventKind::TurnStarted { turn: 0 });
        writer.push(EventKind::Output {
            turn: 0,
            text: "working".into(),
        });
        writer.push(EventKind::RunFinished {
            ok: true,
            exit_code: Some(0),
            timed_out: false,
        });
        writer.seal(true, Some(store)).expect("seal persists")
    }

    #[test]
    fn for_scope_uses_project_nerve_runs() {
        let store = RunStore::for_scope(Some(Path::new("/tmp/proj"))).unwrap();
        assert_eq!(store.dir(), Path::new("/tmp/proj/.nerve/runs"));
    }

    #[test]
    fn writer_seals_persists_and_round_trips() {
        let dir = tempdir().unwrap();
        let store = RunStore::new(dir.path().join("runs"));
        let sealed = sample_writer(&store);

        // run_id is the content address (== root_hash) and is a 64-hex SHA-256.
        assert_eq!(sealed.run_id, sealed.root_hash);
        assert_eq!(sealed.run_id.len(), 64);
        assert_eq!(sealed.event_count, 4);

        // The persisted run reloads identically, and its root_hash matches a pure
        // rebuild over the same events (the store does not perturb the digest).
        let loaded = store.load_record(&sealed.run_id).unwrap();
        assert_eq!(loaded.run_id, sealed.run_id);
        assert_eq!(loaded.schema_version, RUN_SCHEMA_VERSION);
        assert_eq!(loaded.events.len(), 4);
        assert!(loaded.finished);
        let rebuilt = nerve_core::build_run(
            loaded.session_id.clone(),
            loaded.agent.clone(),
            loaded.root.clone(),
            123,
            Some(456),
            true,
            loaded.events.clone(),
        );
        assert_eq!(rebuilt.root_hash, loaded.root_hash);
    }

    #[test]
    fn list_orders_most_recent_first_and_tolerates_missing_dir() {
        let dir = tempdir().unwrap();
        let store = RunStore::new(dir.path().join("runs"));
        assert!(store.list().unwrap().is_empty(), "missing dir is empty");

        for (task, ts) in [("a", 100u64), ("b", 300), ("c", 200)] {
            let mut run = nerve_core::build_run(
                "job",
                "codex",
                None,
                ts,
                Some(ts + 1),
                true,
                vec![Event {
                    seq: 0,
                    kind: EventKind::RunStarted {
                        agent: "codex".into(),
                        task: task.into(),
                        cwd: None,
                    },
                }],
            );
            // Distinct tasks -> distinct content addresses -> distinct files.
            assert!(!run.run_id.is_empty());
            run.started_at_ms = ts;
            store.write_record(&run).unwrap();
        }
        let order: Vec<u64> = store
            .list()
            .unwrap()
            .into_iter()
            .map(|r| r.started_at_ms)
            .collect();
        assert_eq!(order, vec![300, 200, 100]);
    }

    #[test]
    fn run_get_and_list_handlers() {
        let dir = tempdir().unwrap();
        let store = RunStore::new(dir.path().join("runs"));
        let sealed = sample_writer(&store);

        let listed = run_run_list(Some(&store));
        assert_eq!(listed["runs"].as_array().unwrap().len(), 1);
        assert_eq!(listed["runs"][0]["run_id"], json!(sealed.run_id));

        let got = run_run_get(&sealed.run_id, Some(&store)).unwrap();
        assert_eq!(got["run"]["run_id"], json!(sealed.run_id));
        assert_eq!(got["run"]["events"].as_array().unwrap().len(), 4);

        // Unknown id, and a None store, both error / empty.
        assert!(run_run_get("nope", Some(&store)).is_err());
        assert!(run_run_get("x", None).is_err());
        assert_eq!(run_run_list(None)["runs"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn newer_schema_version_is_rejected() {
        let raw = json!({
            "schema_version": 999, "run_id": "r", "session_id": "s", "agent": "codex",
            "started_at_ms": 1, "events": []
        })
        .to_string();
        let err = deserialize_record(&raw).unwrap_err();
        assert!(err.to_string().contains("newer than supported"), "{err}");
    }

    #[test]
    fn invalid_ids_are_rejected() {
        let dir = tempdir().unwrap();
        let store = RunStore::new(dir.path().to_path_buf());
        for bad in ["../escape", "a/b", "", "dots.here"] {
            let mut run = nerve_core::build_run("s", "codex", None, 1, None, true, vec![]);
            run.run_id = bad.to_string();
            assert!(
                store.write_record(&run).is_err(),
                "expected '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn cost_micro_usd_conversion_is_lossless_enough_and_guards_bad_values() {
        assert_eq!(cost_to_micro_usd(Some(0.0123)), Some(12300));
        assert_eq!(cost_to_micro_usd(None), None);
        assert_eq!(cost_to_micro_usd(Some(-1.0)), None);
        assert_eq!(cost_to_micro_usd(Some(f64::NAN)), None);
    }
}
