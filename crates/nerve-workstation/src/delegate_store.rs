//! Durable delegate-session persistence (trust-substrate credibility floor), the
//! sibling of [`FlowStore`](crate::flow_store) and [`SessionStore`](crate::session::SessionStore).
//!
//! North-star §5 keeps live daemon **jobs** in-memory by design, but a cockpit
//! orchestrating long-lived external agents must let a session survive a daemon
//! restart — at least as an INSPECTABLE / AUDITABLE record, and (later) a RESUMABLE
//! one. Like flow ledgers and agent.run transcripts, we persist the **record, not
//! the live subprocess** (the child dies with the daemon): one
//! [`DelegateSessionRecord`] per delegated session, keyed by the `delegate.start`
//! job id, capturing the agent kind, posture, and — crucially — the agent's OWN
//! captured session/thread id (`agent_session_id`), the key a future re-spawn uses
//! to continue the agent's native conversation (`claude --resume <id>` etc.).
//!
//! ```text
//! .nerve/delegates/<session_id>.json   # the versioned DelegateSessionRecord
//! ```
//!
//! Mirrors the verified versioned [`FlowStore`] discipline: a record with
//! `schema_version` + a tolerant [`load_record`](DelegateStore::load_record) path +
//! a [`migrate_to_current`] seam, owned by THIS module so the on-disk schema evolves
//! independently of the protocol/domain types. Writes are **atomic** (temp + rename)
//! and **best-effort** (a persistence failure never fails a delegated turn). The
//! `delegate.list`/`delegate.get` merge ([`run_delegate_list`]/[`run_delegate_get`])
//! shadows a persisted record with its live counterpart, exactly as `flow.list` does.

use crate::delegate_live::LiveSessions;
use anyhow::{Context, Result, anyhow};
use nerve_runtime::{DelegateAutonomy, DelegateRole, RuntimeError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Current on-disk delegate-record schema. Bump when [`DelegateSessionRecord`]
/// changes shape, and add a migration arm in [`migrate_to_current`].
const SCHEMA_VERSION: u32 = 1;

/// A persisted delegated-session record. Versioned exactly like
/// [`FlowRecord`](crate::flow_store) / [`SessionRecord`](crate::session::SessionRecord):
/// `schema_version` first, then the id (== the `delegate.start` job id == filename),
/// with `#[serde(default)]` on every additive field so older records still load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DelegateSessionRecord {
    /// On-disk schema version, for migration. See [`SCHEMA_VERSION`].
    pub(crate) schema_version: u32,
    /// The session id (also the filename and the `delegate.start` job id).
    pub(crate) session_id: String,
    /// The catalog agent kind: `claude` / `codex` / `gemini`.
    pub(crate) agent: String,
    /// The served project root the session ran against.
    #[serde(default)]
    pub(crate) root: Option<String>,
    /// The working directory the agent ran in.
    #[serde(default)]
    pub(crate) cwd: Option<String>,
    /// The effective autonomy posture (`read_only` / `edit` / `full`).
    #[serde(default)]
    pub(crate) autonomy: String,
    /// The behavior role preset (`standard` / `scout`).
    #[serde(default)]
    pub(crate) role: String,
    /// The model override, if any.
    #[serde(default)]
    pub(crate) model: Option<String>,
    /// The agent's OWN captured session/thread id (claude session, codex thread),
    /// persisted once turn 1 establishes it — the key for a future resume-by-id.
    #[serde(default)]
    pub(crate) agent_session_id: Option<String>,
    /// Unix-epoch milliseconds when the session started.
    pub(crate) started_at_ms: u64,
    /// Unix-epoch milliseconds of the last update (None until first).
    #[serde(default)]
    pub(crate) updated_at_ms: Option<u64>,
    /// Whether the session was explicitly closed (vs. orphaned by a daemon exit).
    #[serde(default)]
    pub(crate) finished: bool,
}

impl DelegateSessionRecord {
    /// Begin a fresh record for a starting live delegated session.
    pub(crate) fn begin(
        session_id: &str,
        agent: &str,
        root: &Path,
        cwd: &Path,
        autonomy: DelegateAutonomy,
        role: DelegateRole,
        model: Option<String>,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            session_id: session_id.to_string(),
            agent: agent.to_string(),
            root: Some(root.display().to_string()),
            cwd: Some(cwd.display().to_string()),
            autonomy: autonomy_label(autonomy).to_string(),
            role: role_label(role).to_string(),
            model,
            agent_session_id: None,
            started_at_ms: now_ms(),
            updated_at_ms: None,
            finished: false,
        }
    }

    /// Record the agent's own captured session/thread id (the resume key).
    pub(crate) fn set_agent_session_id(&mut self, agent_session_id: Option<String>) {
        self.agent_session_id = agent_session_id;
        self.updated_at_ms = Some(now_ms());
    }

    /// Mark the session explicitly closed.
    pub(crate) fn mark_closed(&mut self) {
        self.finished = true;
        self.updated_at_ms = Some(now_ms());
    }

    /// Bump the last-activity timestamp (e.g. after a steer turn).
    pub(crate) fn touch(&mut self) {
        self.updated_at_ms = Some(now_ms());
    }
}

/// A directory of persisted delegate sessions (`<dir>/<session_id>.json`). Sibling
/// of [`FlowStore`](crate::flow_store) / [`SessionStore`](crate::session::SessionStore).
#[derive(Clone)]
pub(crate) struct DelegateStore {
    dir: PathBuf,
}

impl DelegateStore {
    /// Wrap an explicit delegates directory.
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Resolve the delegates directory for a scope: `<root>/.nerve/delegates` for a
    /// project root, else the global `config_home()/delegates`.
    pub(crate) fn for_scope(root: Option<&Path>) -> Result<Self> {
        Ok(Self::new(resolve_delegates_dir(root)?))
    }

    /// The backing directory (mirrors `FlowStore::dir`; used by tests).
    #[allow(dead_code, reason = "accessor mirroring FlowStore::dir; used by tests")]
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// The per-session file `<dir>/<session_id>.json` (validating the id stays in-dir).
    fn path_for(&self, session_id: &str) -> Result<PathBuf> {
        validate_id(session_id)?;
        Ok(self.dir.join(format!("{session_id}.json")))
    }

    /// Persist a record atomically (temp + rename), creating the dir on demand.
    pub(crate) fn write_record(&self, record: &DelegateSessionRecord) -> Result<()> {
        let path = self.path_for(&record.session_id)?;
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("failed to create delegates dir {}", self.dir.display()))?;
        let json = serde_json::to_string_pretty(record).context("serialize delegate record")?;
        atomic_write(&path, json.as_bytes())
    }

    /// Load and migrate one record by id.
    pub(crate) fn load_record(&self, session_id: &str) -> Result<DelegateSessionRecord> {
        let path = self.path_for(session_id)?;
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        deserialize_record(&raw).with_context(|| format!("failed to parse delegate {session_id}"))
    }

    /// All persisted records, most recent first (tolerating a missing dir + bad files).
    pub(crate) fn list(&self) -> Result<Vec<DelegateSessionRecord>> {
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
        records.sort_by(|a, b| {
            b.started_at_ms
                .cmp(&a.started_at_ms)
                .then_with(|| b.session_id.cmp(&a.session_id))
        });
        Ok(records)
    }
}

/// Resolve a `delegate.get`. Mirrors `flow.get`: the live registry wins; a session
/// no longer in memory but persisted falls back to the [`DelegateStore`], so it stays
/// inspectable after a daemon restart.
pub(crate) fn run_delegate_get(
    session_id: &str,
    live: &LiveSessions,
    store: Option<&DelegateStore>,
) -> Result<Value, RuntimeError> {
    if let Ok(found) = live.get_snapshot(session_id) {
        return Ok(found);
    }
    let store = store
        .ok_or_else(|| RuntimeError::adapter(format!("no delegate session `{session_id}`")))?;
    let record = store.load_record(session_id).map_err(|err| {
        RuntimeError::adapter(format!("no delegate session `{session_id}`: {err}"))
    })?;
    Ok(json!({ "delegate": persisted_delegate_snapshot(&record) }))
}

/// Resolve a `delegate.list`. Mirrors `flow.list`: merges the live registry with the
/// persisted store, de-duplicating by id (a live session shadows its persisted
/// record), so a client sees both running and past sessions across restarts.
pub(crate) fn run_delegate_list(live: &LiveSessions, store: Option<&DelegateStore>) -> Value {
    let live_val = live.list();
    let mut entries: Vec<Value> = live_val
        .get("delegates")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(store) = store
        && let Ok(records) = store.list()
    {
        let live_ids = live_delegate_ids(&entries);
        for record in records {
            if !live_ids.contains(&record.session_id) {
                entries.push(persisted_delegate_snapshot(&record));
            }
        }
    }
    json!({ "delegates": entries })
}

/// The set of session ids already present in the live list (so a persisted record for
/// a still-live session is not listed twice).
fn live_delegate_ids(entries: &[Value]) -> HashSet<String> {
    entries
        .iter()
        .filter_map(|e| {
            e.get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// Project a persisted record onto the same JSON shape a live snapshot uses, so a
/// client renders persisted and live sessions uniformly. A persisted session is never
/// live, so its status is `ended` (explicitly closed) or `orphaned` (the daemon exited
/// while it was running); `persisted: true` marks the provenance.
fn persisted_delegate_snapshot(record: &DelegateSessionRecord) -> Value {
    json!({
        "session_id": record.session_id,
        "agent": record.agent,
        "status": if record.finished { "ended" } else { "orphaned" },
        "agent_session_id": record.agent_session_id,
        "started_at_ms": record.started_at_ms,
        "updated_at_ms": record.updated_at_ms,
        "persisted": true,
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
            .unwrap_or("delegate-write")
    ));
    fs::write(&tmp, bytes).with_context(|| format!("failed to write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("failed to rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Parse + migrate a record, tolerant of an older/missing `schema_version` (treated as
/// v1); rejects a newer-than-supported version.
fn deserialize_record(raw: &str) -> Result<DelegateSessionRecord> {
    let mut value: Value = serde_json::from_str(raw).context("invalid delegate JSON")?;
    let version = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    migrate_to_current(&mut value, version)?;
    serde_json::from_value(value).context("delegate record shape mismatch")
}

/// Upgrade a record `value` from `version` to [`SCHEMA_VERSION`] in place. Only one
/// version exists today, so this is the newer-than-known guard + a re-stamp; add an
/// arm per future bump (mirrors `FlowStore` / `SessionStore`).
fn migrate_to_current(value: &mut Value, version: u64) -> Result<()> {
    if version > u64::from(SCHEMA_VERSION) {
        return Err(anyhow!(
            "delegate schema_version {version} is newer than supported {SCHEMA_VERSION}; upgrade nerve"
        ));
    }
    if let Some(object) = value.as_object_mut() {
        object.insert("schema_version".into(), Value::from(SCHEMA_VERSION));
    }
    Ok(())
}

/// `<root>/.nerve/delegates` for a project root, else the global `config_home()/delegates`.
fn resolve_delegates_dir(root: Option<&Path>) -> Result<PathBuf> {
    match root {
        Some(root) => Ok(root.join(".nerve").join("delegates")),
        None => {
            let home = nerve_agent::auth::config_home().map_err(|err| anyhow!("{err}"))?;
            Ok(home.join("delegates"))
        }
    }
}

/// Reject ids that could escape the delegates directory (same token rule as the other
/// stores: ASCII alphanumerics plus `-`/`_`).
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
            "invalid delegate id '{id}': use only letters, digits, '-' and '_'"
        ))
    }
}

/// The serde wire label for an autonomy posture (kept in sync with the enum's
/// `rename_all = "snake_case"`).
fn autonomy_label(autonomy: DelegateAutonomy) -> &'static str {
    match autonomy {
        DelegateAutonomy::ReadOnly => "read_only",
        DelegateAutonomy::Edit => "edit",
        DelegateAutonomy::Full => "full",
    }
}

/// The serde wire label for a behavior role.
fn role_label(role: DelegateRole) -> &'static str {
    match role {
        DelegateRole::Standard => "standard",
        DelegateRole::Scout => "scout",
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

    fn record(session_id: &str) -> DelegateSessionRecord {
        DelegateSessionRecord::begin(
            session_id,
            "claude",
            Path::new("/tmp/proj"),
            Path::new("/tmp/proj/sub"),
            DelegateAutonomy::Edit,
            DelegateRole::Scout,
            Some("opus".into()),
        )
    }

    #[test]
    fn for_scope_uses_project_nerve_delegates() {
        let store = DelegateStore::for_scope(Some(Path::new("/tmp/proj"))).unwrap();
        assert_eq!(store.dir(), Path::new("/tmp/proj/.nerve/delegates"));
    }

    #[test]
    fn record_round_trips_with_updates() {
        let dir = tempdir().unwrap();
        let store = DelegateStore::new(dir.path().join("delegates"));
        let mut rec = record("job-1");
        store.write_record(&rec).unwrap();

        let loaded = store.load_record("job-1").unwrap();
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
        assert_eq!(loaded.session_id, "job-1");
        assert_eq!(loaded.agent, "claude");
        assert_eq!(loaded.autonomy, "edit");
        assert_eq!(loaded.role, "scout");
        assert_eq!(loaded.model.as_deref(), Some("opus"));
        assert!(loaded.agent_session_id.is_none());
        assert!(!loaded.finished);

        // Capturing the agent's own id then closing round-trips.
        rec.set_agent_session_id(Some("claude-sess-abc".into()));
        rec.mark_closed();
        store.write_record(&rec).unwrap();
        let loaded = store.load_record("job-1").unwrap();
        assert_eq!(loaded.agent_session_id.as_deref(), Some("claude-sess-abc"));
        assert!(loaded.finished);
        assert!(loaded.updated_at_ms.is_some());
    }

    #[test]
    fn list_orders_most_recent_first_and_tolerates_missing_dir() {
        let dir = tempdir().unwrap();
        let store = DelegateStore::new(dir.path().join("delegates"));
        assert!(store.list().unwrap().is_empty(), "missing dir is empty");

        for (id, ts) in [("d-1", 100u64), ("d-2", 300), ("d-3", 200)] {
            let mut rec = record(id);
            rec.started_at_ms = ts;
            store.write_record(&rec).unwrap();
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
    fn newer_schema_version_is_rejected() {
        let raw = json!({
            "schema_version": 999, "session_id": "d", "agent": "claude", "started_at_ms": 1
        })
        .to_string();
        let err = deserialize_record(&raw).unwrap_err();
        assert!(err.to_string().contains("newer than supported"), "{err}");
    }

    #[test]
    fn missing_schema_version_loads_as_v1() {
        let raw = json!({ "session_id": "d", "agent": "codex", "started_at_ms": 1 }).to_string();
        let loaded = deserialize_record(&raw).unwrap();
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
        assert!(!loaded.finished);
        assert!(loaded.agent_session_id.is_none());
    }

    #[test]
    fn invalid_ids_are_rejected() {
        let dir = tempdir().unwrap();
        let store = DelegateStore::new(dir.path().to_path_buf());
        for bad in ["../escape", "a/b", "", "dots.here"] {
            let mut rec = record("ok");
            rec.session_id = bad.to_string();
            assert!(
                store.write_record(&rec).is_err(),
                "expected '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn atomic_write_leaves_no_temp_file() {
        let dir = tempdir().unwrap();
        let store = DelegateStore::new(dir.path().join("delegates"));
        store.write_record(&record("job-1")).unwrap();
        let temps: Vec<_> = fs::read_dir(dir.path().join("delegates"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(temps.is_empty(), "no temp file should remain after rename");
    }

    #[test]
    fn list_merge_includes_persisted_when_not_live() {
        let dir = tempdir().unwrap();
        let store = DelegateStore::new(dir.path().join("delegates"));
        let mut rec = record("gone-1");
        rec.mark_closed();
        store.write_record(&rec).unwrap();

        // An empty live registry => the persisted record surfaces as ended+persisted.
        let live = LiveSessions::default();
        let listed = run_delegate_list(&live, Some(&store));
        let delegates = listed["delegates"].as_array().unwrap();
        assert_eq!(delegates.len(), 1);
        assert_eq!(delegates[0]["session_id"], json!("gone-1"));
        assert_eq!(delegates[0]["status"], json!("ended"));
        assert_eq!(delegates[0]["persisted"], json!(true));

        // get falls back to the store for a non-live id; unknown id errors.
        let got = run_delegate_get("gone-1", &live, Some(&store)).unwrap();
        assert_eq!(got["delegate"]["session_id"], json!("gone-1"));
        assert!(run_delegate_get("nope", &live, Some(&store)).is_err());
    }

    #[test]
    fn orphaned_status_for_an_unclosed_persisted_record() {
        let snap = persisted_delegate_snapshot(&record("d"));
        assert_eq!(snap["status"], json!("orphaned"));
        assert_eq!(snap["persisted"], json!(true));
    }
}
