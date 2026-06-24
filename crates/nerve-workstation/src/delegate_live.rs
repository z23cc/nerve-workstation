//! DA-5a: the live-session registry that turns the one-shot `delegate.start` JOB
//! into a persistent, steerable session.
//!
//! The DA-2 `delegate.start` job spawns a CLI, streams to completion, and the job
//! goes terminal. DA-5a keeps a `claude` [`DelegateSession`] **alive** after turn
//! 1: the `delegate.start` job thread *parks* (status stays `running`) holding the
//! live session in this registry, while separate `delegate.steer` / `delegate.close`
//! commands reach in by `session_id` (== the start job's id) to run further turns
//! or end it. The job becomes terminal only when the session is closed (explicit
//! close, job cancel, or the child exiting).
//!
//! Concurrency: each registered session is an [`Arc<LiveHandle>`]; a turn runs
//! under the handle's `session` mutex, so a steer can never overlap turn 1 or
//! another steer. The close signal is a `Condvar` the parked start thread waits on.

use crate::delegate_session::DelegateSession;
use crate::delegate_session_codex::CodexSession;
use nerve_core::CancelToken;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// How often the cancel-linking watcher wakes to fan a source-token cancellation
/// into the per-turn combined token. Matches the turn loops' own poll cadence, so a
/// close/cancel is observed within roughly one poll interval.
const LINK_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Links two source [`CancelToken`]s into one combined token a turn loop polls. A
/// watcher thread cancels the combined token as soon as *either* source fires, so a
/// per-turn cancel (the steer job's own token) and the session-scoped cancel (an
/// explicit close / start-job cancel) both interrupt the running turn. Dropping the
/// link stops the watcher (it is no longer needed once the turn returns).
///
/// `CancelToken` is a bare atomic bool with no native "linked"/"any-of" combinator,
/// and the turn loops + proxy + interrupt all take a concrete `&CancelToken`; a tiny
/// watcher is the way to OR two sources without changing those signatures.
struct CancelLink {
    combined: CancelToken,
    stop: Arc<AtomicBool>,
    watcher: Option<std::thread::JoinHandle<()>>,
}

impl CancelLink {
    /// Spawn a watcher that fans `per_turn` and `session` cancellation into a fresh
    /// combined token. If either source is already cancelled the combined token is
    /// pre-cancelled, so the turn loop sees it on its first check.
    fn spawn(per_turn: CancelToken, session: CancelToken) -> Self {
        let combined = CancelToken::new();
        if per_turn.is_cancelled() || session.is_cancelled() {
            combined.cancel();
        }
        let stop = Arc::new(AtomicBool::new(false));
        let watcher = {
            let combined = combined.clone();
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    if per_turn.is_cancelled() || session.is_cancelled() {
                        combined.cancel();
                        return;
                    }
                    std::thread::sleep(LINK_POLL_INTERVAL);
                }
            })
        };
        Self {
            combined,
            stop,
            watcher: Some(watcher),
        }
    }

    /// The combined token to drive the turn under.
    fn token(&self) -> &CancelToken {
        &self.combined
    }
}

impl Drop for CancelLink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
    }
}

/// A live persistent delegate driver — one per protocol. Both variants own a
/// [`PersistentChild`](crate::sandbox::PersistentChild) and run one turn per
/// message; this enum lets the [`LiveHandle`] registry hold either without
/// duplicating the parking/close machinery. Steer/close dispatch to the variant.
pub(crate) enum LiveDriver {
    /// claude over `stream-json` (DA-5a/5b).
    Claude(DelegateSession),
    /// codex over `app-server` JSON-RPC (DA-5c).
    Codex(CodexSession),
}

impl LiveDriver {
    /// The catalog agent name this driver speaks (for progress events + result
    /// JSON), so a steer reports the same agent the start did.
    pub(crate) fn agent(&self) -> &'static str {
        match self {
            Self::Claude(_) => "claude",
            Self::Codex(_) => "codex",
        }
    }

    /// The agent's own session handle (claude's `session_id`, codex's thread id) if
    /// it has been captured at start; otherwise `None` (the caller falls back to the
    /// start-job id, which is the registry key).
    pub(crate) fn session_id(&self) -> Option<&str> {
        match self {
            Self::Claude(session) => session.session_id(),
            Self::Codex(session) => session.thread_id(),
        }
    }

    /// Run one more turn on the live child, forwarding streamed text to
    /// `on_progress`.
    fn steer(
        &mut self,
        message: &str,
        cancel: &nerve_core::CancelToken,
        on_progress: &mut dyn FnMut(&str),
    ) -> Result<crate::delegate_session::TurnResult, crate::delegate_session::SessionError> {
        match self {
            Self::Claude(session) => session.steer(message, cancel, on_progress),
            Self::Codex(session) => session.steer(message, cancel, on_progress),
        }
    }

    /// Tear the child down (close stdin / reap, force-kill on a stuck child).
    pub(crate) fn close(&mut self) {
        match self {
            Self::Claude(session) => session.close(),
            Self::Codex(session) => session.close(),
        }
    }
}

/// One live delegated session, keyed by the originating `delegate.start` job id.
pub(crate) struct LiveHandle {
    /// The catalog agent kind ("claude"/"codex"), cached at construction and fixed
    /// for the session's life. Held outside the `session` mutex so a read-only
    /// `delegate.list`/`delegate.get` snapshot can report the agent without locking
    /// (and thus without blocking on an in-flight turn that holds that mutex).
    agent_kind: &'static str,
    /// The live driver. `None` once closed/reaped, so a late steer sees a clear
    /// "closed" error rather than touching a dead child.
    session: Mutex<Option<LiveDriver>>,
    /// Set when close (or cancel) is requested; the parked start thread waits on
    /// `close_cv` for it to flip, then tears the session down.
    close_requested: Mutex<bool>,
    close_cv: Condvar,
    /// Session-scoped cancellation, fired by [`Self::request_close`] (an explicit
    /// `delegate.close` or a job cancel). Every turn — start turn 1 and each steer —
    /// runs under a token linked to this one, so a close/cancel promptly interrupts
    /// the in-flight turn and reaps the child instead of waiting out the turn timeout.
    session_cancel: CancelToken,
}

impl LiveHandle {
    fn new(session: LiveDriver) -> Self {
        Self {
            agent_kind: session.agent(),
            session: Mutex::new(Some(session)),
            close_requested: Mutex::new(false),
            close_cv: Condvar::new(),
            session_cancel: CancelToken::new(),
        }
    }

    /// A non-blocking, read-only summary of this session for `delegate.list` /
    /// `delegate.get`: `{ session_id, agent, status, agent_session_id }`.
    ///
    /// `agent` is the cached driver kind. `status` is `live` (driver present),
    /// `closed` (driver reaped), or `busy` (a turn currently holds the session
    /// lock) — read via `try_lock`, so snapshotting the fleet NEVER blocks on an
    /// in-flight turn (which holds the `session` mutex for its whole duration).
    /// `agent_session_id` is the agent's own captured id (claude session / codex
    /// thread) when the driver is reachable, for later resume-by-id.
    pub(crate) fn snapshot(&self, session_id: &str) -> Value {
        let summarize = |slot: &Option<LiveDriver>| match slot.as_ref() {
            Some(driver) => ("live", driver.session_id().map(str::to_string)),
            None => ("closed", None),
        };
        let (status, agent_session_id) = match self.session.try_lock() {
            Ok(slot) => summarize(&slot),
            Err(std::sync::TryLockError::Poisoned(poison)) => summarize(&poison.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => ("busy", None),
        };
        json!({
            "session_id": session_id,
            "agent": self.agent_kind,
            "status": status,
            "agent_session_id": agent_session_id,
        })
    }

    /// The catalog agent name the live driver speaks, or `Err(closed)` if the
    /// session was already torn down. Fixed for the session's life.
    pub(crate) fn agent(&self) -> Result<&'static str, LiveError> {
        crate::sync::lock_recover(&self.session)
            .as_ref()
            .map(LiveDriver::agent)
            .ok_or(LiveError::Closed)
    }

    /// Run one steer turn under the session lock, forwarding assistant text to
    /// `on_progress`. Returns `Err(closed)` if the session was already torn down.
    /// The turn names whichever agent the live driver speaks.
    ///
    /// The turn runs under a token linked to the session-scoped cancel, so a
    /// `request_close()` (explicit close or job cancel) that lands mid-steer
    /// interrupts the running turn promptly. A cancelled/interrupted turn TEARS THE
    /// SESSION DOWN (reaps the child, clears the slot), matching the start-cancel
    /// semantics — a later steer then sees a clear "no live session" rather than
    /// reading the in-flight turn's undrained lines (which would desync the next turn).
    pub(crate) fn steer(
        &self,
        message: &str,
        cancel: &nerve_core::CancelToken,
        on_progress: &mut dyn FnMut(&str),
    ) -> Result<(crate::delegate_session::TurnResult, &'static str), LiveError> {
        let mut guard = crate::sync::lock_recover(&self.session);
        let session = guard.as_mut().ok_or(LiveError::Closed)?;
        let agent = session.agent();
        // Link the per-turn token to the session-scoped cancel so a close/cancel
        // during this turn fires the token the turn loop is polling.
        let link = CancelLink::spawn(cancel.clone(), self.session_cancel.clone());
        let result = session.steer(message, link.token(), on_progress);
        drop(link);
        match result {
            Ok(turn) => Ok((turn, agent)),
            // A cancelled/interrupted turn leaves the session half-consumed. Tear it
            // down here (reap + clear the slot) so it is no longer steerable, and
            // signal close so the parked start thread wakes and the start job goes
            // terminal rather than parking forever on a now-dead child.
            Err(crate::delegate_session::SessionError::Cancelled) => {
                if let Some(mut driver) = guard.take() {
                    driver.close();
                }
                drop(guard);
                self.request_close();
                Err(LiveError::Interrupted)
            }
            // A timed-out or process-exited turn is session-fatal: the child is
            // either stalled (timeout) or already dead (exit), and its undrained
            // result/deltas would bleed into the next steer. Reap + clear the slot
            // and signal close so the parked start thread wakes and the start job
            // goes terminal, then still surface the failure (not a cancellation).
            Err(
                err @ (crate::delegate_session::SessionError::TurnTimedOut
                | crate::delegate_session::SessionError::ProcessExited),
            ) => {
                if let Some(mut driver) = guard.take() {
                    driver.close();
                }
                drop(guard);
                self.request_close();
                Err(LiveError::Session(err.to_string()))
            }
            Err(err) => Err(LiveError::Session(err.to_string())),
        }
    }

    /// Signal the parked start thread to close: flip the flag and wake it. Also
    /// fires the session-scoped cancel so any in-flight turn (a steer holding the
    /// session lock) is interrupted promptly rather than running to its timeout.
    pub(crate) fn request_close(&self) {
        self.session_cancel.cancel();
        let mut requested = crate::sync::lock_recover(&self.close_requested);
        *requested = true;
        self.close_cv.notify_all();
    }

    /// Block until close is requested (by [`Self::request_close`]). Called by the
    /// parked `delegate.start` thread after turn 1.
    fn wait_for_close(&self) {
        let mut requested = crate::sync::lock_recover(&self.close_requested);
        while !*requested {
            requested = self
                .close_cv
                .wait(requested)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Tear the live session down (close stdin / reap, or force-kill on cancel).
    /// Idempotent: a second call finds the session already taken.
    fn shutdown(&self) {
        if let Some(mut driver) = crate::sync::lock_recover(&self.session).take() {
            driver.close();
        }
    }
}

/// A live-session lookup/operation failure surfaced to a steer/close caller.
#[derive(Debug)]
pub(crate) enum LiveError {
    /// No live session is registered under the given id.
    Unknown(String),
    /// The session was already closed (its child reaped).
    Closed,
    /// The in-flight turn was interrupted (a per-turn cancel or a session-scoped
    /// close), and the session was torn down. The caller reports this as a
    /// cancellation rather than a failure, regardless of its own job token.
    Interrupted,
    /// A turn-level failure from the underlying [`DelegateSession`].
    Session(String),
}

impl LiveError {
    /// Whether this error is an in-flight cancellation/interruption (so the job
    /// finishes `job_cancelled`) rather than a plain failure.
    pub(crate) fn is_cancellation(&self) -> bool {
        matches!(self, Self::Interrupted)
    }
}

impl std::fmt::Display for LiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(id) => write!(f, "no live delegated session `{id}` (it may have ended)"),
            Self::Closed => write!(f, "delegated session is already closed"),
            Self::Interrupted => write!(f, "delegated turn was interrupted"),
            Self::Session(message) => write!(f, "{message}"),
        }
    }
}

/// The registry of live delegated sessions held by the [`JobManager`](crate::jobs).
#[derive(Default)]
pub(crate) struct LiveSessions {
    sessions: Mutex<HashMap<String, Arc<LiveHandle>>>,
}

impl LiveSessions {
    /// Register a freshly-started session under its start-job id, returning the
    /// shared handle the parked thread parks on.
    pub(crate) fn register(&self, session_id: &str, driver: LiveDriver) -> Arc<LiveHandle> {
        let handle = Arc::new(LiveHandle::new(driver));
        crate::sync::lock_recover(&self.sessions)
            .insert(session_id.to_string(), Arc::clone(&handle));
        handle
    }

    /// Look up a registered session by id (for a steer/close routed as its own
    /// command).
    pub(crate) fn get(&self, session_id: &str) -> Result<Arc<LiveHandle>, LiveError> {
        crate::sync::lock_recover(&self.sessions)
            .get(session_id)
            .cloned()
            .ok_or_else(|| LiveError::Unknown(session_id.to_string()))
    }

    /// Park the start thread until close is requested, then shut the session down
    /// and deregister it. Holding the `Arc` keeps the handle alive for steers even
    /// though it's removed from the map at the end.
    pub(crate) fn park_until_closed(&self, session_id: &str, handle: &Arc<LiveHandle>) {
        handle.wait_for_close();
        handle.shutdown();
        crate::sync::lock_recover(&self.sessions).remove(session_id);
    }

    /// Request close + deregister for an explicit `delegate.close` or a job cancel.
    /// Returns whether a session was found (so close can report unknown ids).
    pub(crate) fn close(&self, session_id: &str) -> Result<(), LiveError> {
        let handle = self.get(session_id)?;
        handle.request_close();
        Ok(())
    }

    /// Snapshot every live delegated session for `delegate.list`, sorted by id for
    /// deterministic output: `{ "delegates": [ {…}, … ] }`. Clones the handle
    /// `Arc`s under the registry lock then snapshots each WITHOUT holding it (and
    /// each snapshot is itself non-blocking), so a long in-flight turn can never
    /// stall the listing or the registry. Mirrors `flow.list` / `session.list`.
    pub(crate) fn list(&self) -> Value {
        let mut entries: Vec<(String, Arc<LiveHandle>)> = crate::sync::lock_recover(&self.sessions)
            .iter()
            .map(|(id, handle)| (id.clone(), Arc::clone(handle)))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let delegates: Vec<Value> = entries
            .iter()
            .map(|(id, handle)| handle.snapshot(id))
            .collect();
        json!({ "delegates": delegates })
    }

    /// Snapshot one live delegated session by id for `delegate.get`:
    /// `{ "delegate": {…} }`. An unknown id is an error. Mirrors `flow.get` /
    /// `session.get`.
    pub(crate) fn get_snapshot(&self, session_id: &str) -> Result<Value, LiveError> {
        let handle = self.get(session_id)?;
        Ok(json!({ "delegate": handle.snapshot(session_id) }))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::delegate_session::DelegateSession;
    use crate::sandbox::{CommandSpec, PersistentChild};

    /// Whether a process group is still alive (`killpg(pgid, 0)` succeeds). A reaped
    /// group returns `ESRCH`, so this goes false once the child is gone.
    fn group_alive(pgid: u32) -> bool {
        // SAFETY: signal 0 performs only the existence/permission check, no delivery.
        unsafe { libc::killpg(pgid as libc::pid_t, 0) == 0 }
    }

    /// Spawn a long-lived contained child (a `sleep` that ignores stdin EOF) as a
    /// `PersistentChild`, so a test can assert teardown actually reaps it.
    fn spawn_sleeper() -> PersistentChild {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = crate::delegate_runtime::delegate_policy(dir.path());
        // `sleep 600 </dev/null` never exits on stdin EOF, so a bare drop would leak
        // it — only an explicit kill of the group reaps it.
        let spec = CommandSpec {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "exec sleep 600".to_string()],
        };
        // Keep `dir` alive for the child's lifetime by leaking it (test-scoped).
        std::mem::forget(dir);
        PersistentChild::spawn(&spec, &policy).expect("spawn sleeper")
    }

    /// Spawn a contained child whose stdout is already closed (`exec 1>&-`) but
    /// which keeps reading stdin (`exec cat >/dev/null`). The reader thread sees
    /// immediate EOF on stdout (so a turn's `recv` disconnects -> `ProcessExited`)
    /// while the child stays alive holding the stdin pipe `PersistentChild` owns —
    /// the exact precondition for a steer that fails with `ProcessExited`.
    fn spawn_stdout_closed_child() -> PersistentChild {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = crate::delegate_runtime::delegate_policy(dir.path());
        let spec = CommandSpec {
            command: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "exec 1>&-; exec cat >/dev/null".to_string(),
            ],
        };
        std::mem::forget(dir);
        PersistentChild::spawn(&spec, &policy).expect("spawn stdout-closed child")
    }

    /// Finding 7: a steer whose turn fails with `ProcessExited` (stdout EOF) must
    /// TEAR THE SESSION DOWN — clear the live slot and request close — not leave the
    /// dead/desynced child registered. Before the fix the `ProcessExited` arm fell
    /// through the catch-all and returned `LiveError::Session` without taking the
    /// driver slot or calling `request_close()`, so the session stayed steerable and
    /// the parked start thread never woke.
    #[test]
    fn steer_tears_down_session_when_turn_process_exits() {
        let session = DelegateSession::from_child_for_test(spawn_stdout_closed_child());
        let pgid = session.child_pid();
        let handle = LiveHandle::new(LiveDriver::Claude(session));

        let cancel = CancelToken::new();
        let mut sink = |_: &str| {};
        let err = handle
            .steer("hello", &cancel, &mut sink)
            .expect_err("a process-exited turn must surface an error");

        // The failure surfaces as a plain session failure, NOT a cancellation, so the
        // job maps it to job_failed rather than job_cancelled.
        assert!(
            matches!(err, LiveError::Session(_)),
            "expected LiveError::Session, got {err:?}"
        );
        assert!(!err.is_cancellation());

        // The session slot is cleared: a follow-up steer sees a closed session rather
        // than reading the dead child's undrained lines.
        assert!(
            matches!(handle.agent(), Err(LiveError::Closed)),
            "session slot should be torn down after ProcessExited"
        );
        // And close was requested, so a parked start thread would wake and terminate.
        assert!(*crate::sync::lock_recover(&handle.close_requested));

        // The torn-down driver reaped the child group.
        for _ in 0..200 {
            if !group_alive(pgid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("process group {pgid} leaked after ProcessExited teardown");
    }

    /// Finding B: the live driver's `close()` must reap the spawned child (close
    /// stdin, then force-kill the group on a child that ignores EOF) — a bare drop
    /// does NOT kill the process group. This is the teardown the `run_delegate_live`
    /// early-return (cancel between turn-1 success and registration) now invokes.
    #[test]
    fn live_driver_close_reaps_the_child_process_group() {
        let session = DelegateSession::from_child_for_test(spawn_sleeper());
        let pgid = session.child_pid();
        assert!(group_alive(pgid), "sleeper should be alive before close");

        let mut driver = LiveDriver::Claude(session);
        driver.close();

        // The group is reaped promptly (close force-kills after a brief EOF window).
        for _ in 0..200 {
            if !group_alive(pgid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("process group {pgid} leaked after close()");
    }

    /// `delegate.list` / `delegate.get` expose the parked fleet over the protocol:
    /// `list()` returns one `{ session_id, agent, status }` per registered session,
    /// `get_snapshot()` returns the same entry under `"delegate"`, an unknown id is
    /// `LiveError::Unknown`, and a torn-down driver reports `status: "closed"`.
    #[test]
    fn list_and_get_snapshot_report_registered_sessions() {
        let registry = LiveSessions::default();
        let session = DelegateSession::from_child_for_test(spawn_sleeper());
        let pgid = session.child_pid();
        let handle = registry.register("job-1", LiveDriver::Claude(session));

        let list = registry.list();
        let delegates = list["delegates"].as_array().expect("delegates array");
        assert_eq!(delegates.len(), 1);
        assert_eq!(delegates[0]["session_id"], json!("job-1"));
        assert_eq!(delegates[0]["agent"], json!("claude"));
        assert_eq!(delegates[0]["status"], json!("live"));

        let got = registry.get_snapshot("job-1").expect("known session");
        assert_eq!(got["delegate"]["session_id"], json!("job-1"));
        assert_eq!(got["delegate"]["agent"], json!("claude"));
        assert!(matches!(
            registry.get_snapshot("nope"),
            Err(LiveError::Unknown(_))
        ));

        // Reap the sleeper (no leak) and confirm the slot then reports `closed`.
        handle.shutdown();
        let after = registry.get_snapshot("job-1").expect("still registered");
        assert_eq!(after["delegate"]["status"], json!("closed"));

        for _ in 0..200 {
            if !group_alive(pgid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("process group {pgid} leaked after shutdown");
    }
}
