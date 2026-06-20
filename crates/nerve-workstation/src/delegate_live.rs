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
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

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
    fn close(&mut self) {
        match self {
            Self::Claude(session) => session.close(),
            Self::Codex(session) => session.close(),
        }
    }
}

/// One live delegated session, keyed by the originating `delegate.start` job id.
pub(crate) struct LiveHandle {
    /// The live driver. `None` once closed/reaped, so a late steer sees a clear
    /// "closed" error rather than touching a dead child.
    session: Mutex<Option<LiveDriver>>,
    /// Set when close (or cancel) is requested; the parked start thread waits on
    /// `close_cv` for it to flip, then tears the session down.
    close_requested: Mutex<bool>,
    close_cv: Condvar,
}

impl LiveHandle {
    fn new(session: LiveDriver) -> Self {
        Self {
            session: Mutex::new(Some(session)),
            close_requested: Mutex::new(false),
            close_cv: Condvar::new(),
        }
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
    pub(crate) fn steer(
        &self,
        message: &str,
        cancel: &nerve_core::CancelToken,
        on_progress: &mut dyn FnMut(&str),
    ) -> Result<(crate::delegate_session::TurnResult, &'static str), LiveError> {
        let mut guard = crate::sync::lock_recover(&self.session);
        let session = guard.as_mut().ok_or(LiveError::Closed)?;
        let agent = session.agent();
        let turn = session
            .steer(message, cancel, on_progress)
            .map_err(|err| LiveError::Session(err.to_string()))?;
        Ok((turn, agent))
    }

    /// Signal the parked start thread to close: flip the flag and wake it.
    pub(crate) fn request_close(&self) {
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
    /// A turn-level failure from the underlying [`DelegateSession`].
    Session(String),
}

impl std::fmt::Display for LiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(id) => write!(f, "no live delegated session `{id}` (it may have ended)"),
            Self::Closed => write!(f, "delegated session is already closed"),
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
}
