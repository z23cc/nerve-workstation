//! The live-flow worker registry — `flow.steer`'s lookup surface (Wave C3a).
//!
//! `flow.steer` (design §4) injects a follow-up turn into a *live* flow branch.
//! The deterministic engine ([`crate::flow`]) is synchronous, so to make a node's
//! worker reachable mid-flight from a *separate* `flow.steer` job thread, the
//! driver registers each steerable node's live [`WorkerSession`] here while it is
//! the flow's current frontier. This is the flow analogue of
//! [`LiveSessions`](crate::delegate_live::LiveSessions)/`LiveHandle` for delegated
//! sessions — but a flow node runs synchronously in the engine loop, so the
//! registry only needs the live session handle (no parking/condvar).
//!
//! ## Steer semantics (design §4, C3a)
//!
//! - A `Single` or `Pipeline` stage's worker is registered as the frontier while it
//!   is the current node; only that frontier is steerable. A `Parallel` wave does
//!   NOT register branches (steering "one of N concurrent branches" is out of scope
//!   for C3a — the engine has no single live frontier there).
//! - [`SteerRegistry::steer`] runs ONE more turn on the looked-up session via the
//!   C0 [`WorkerSession::steer`] port. A worker that is one-shot (a remote/MCP
//!   worker) returns [`WorkerError::NotSteerable`]; a closed/advanced frontier returns a clear
//!   "no live branch" error.
//! - The steered turn's events + final [`TurnResult`] are recorded into the same
//!   [`WorkerLedger`](crate::worker::WorkerLedger) as the original turn, so the
//!   follow-up is part of the replay tape (recorded nondeterminism, design §5).

use super::{TurnResult, WorkerError, WorkerEvent, WorkerLedger, WorkerSession};
use nerve_core::CancelToken;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A live worker session held for steering, keyed by node id within one flow. The
/// session is behind a [`Mutex`] so a `flow.steer` turn and the driver's teardown
/// never touch it concurrently; `None` once closed, so a late steer sees a clear
/// "closed" error rather than a dead session.
struct LiveWorker {
    session: Mutex<Option<Box<dyn WorkerSession>>>,
}

impl LiveWorker {
    fn new(session: Box<dyn WorkerSession>) -> Self {
        Self {
            session: Mutex::new(Some(session)),
        }
    }
}

/// Why a `flow.steer` could not be applied (distinct from a turn that ran but
/// reported `ok=false`).
#[derive(Debug)]
pub(crate) enum SteerError {
    /// No live branch matched the selector (the flow has no steerable frontier, or
    /// the named node is not currently live).
    NoLiveBranch(String),
    /// The selector was unset but more than one branch is live (ambiguous).
    Ambiguous(usize),
    /// The matched worker is one-shot (e.g. a remote/MCP worker) or otherwise not steerable.
    NotSteerable,
    /// The steer turn itself failed mid-flight.
    Turn(String),
}

impl std::fmt::Display for SteerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoLiveBranch(selector) => {
                write!(
                    f,
                    "no live flow branch for {selector} (it may have advanced or finished)"
                )
            }
            Self::Ambiguous(count) => write!(
                f,
                "{count} branches are live; specify target.node_id to steer a specific one"
            ),
            Self::NotSteerable => {
                write!(f, "this flow branch is one-shot and cannot be steered")
            }
            Self::Turn(message) => write!(f, "steer turn failed: {message}"),
        }
    }
}

/// The per-flow registry of live, steerable worker frontiers. Shared between the
/// driver (which registers/closes frontiers) and the `flow.steer` command path
/// (which looks one up and steers it).
#[derive(Default)]
pub(crate) struct SteerRegistry {
    workers: Mutex<HashMap<String, Arc<LiveWorker>>>,
}

impl SteerRegistry {
    /// A fresh, empty registry.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Register `node_id`'s live session as a steerable frontier, taking ownership
    /// of the session handle.
    pub(crate) fn register(&self, node_id: &str, session: Box<dyn WorkerSession>) {
        crate::sync::lock_recover(&self.workers)
            .insert(node_id.to_string(), Arc::new(LiveWorker::new(session)));
    }

    /// Close and deregister `node_id`'s frontier (the driver calls this when a node
    /// is done / the next stage advances). Idempotent.
    pub(crate) fn close(&self, node_id: &str) {
        let removed = crate::sync::lock_recover(&self.workers).remove(node_id);
        if let Some(worker) = removed
            && let Some(mut session) = crate::sync::lock_recover(&worker.session).take()
        {
            session.close();
        }
    }

    /// Close and deregister EVERY live frontier (driver teardown at flow end).
    pub(crate) fn close_all(&self) {
        let drained: Vec<Arc<LiveWorker>> = crate::sync::lock_recover(&self.workers)
            .drain()
            .map(|(_, w)| w)
            .collect();
        for worker in drained {
            if let Some(mut session) = crate::sync::lock_recover(&worker.session).take() {
                session.close();
            }
        }
    }

    /// Resolve a selector (an explicit node id, or "the only live frontier" when
    /// `None`) to its node id + live worker handle in ONE lock pass, erroring if
    /// nothing matches or the unset selector is ambiguous.
    fn resolve(&self, node_id: Option<&str>) -> Result<(String, Arc<LiveWorker>), SteerError> {
        let workers = crate::sync::lock_recover(&self.workers);
        match node_id {
            Some(id) => workers
                .get(id)
                .map(|worker| (id.to_string(), Arc::clone(worker)))
                .ok_or_else(|| SteerError::NoLiveBranch(format!("node `{id}`"))),
            None => match workers.len() {
                0 => Err(SteerError::NoLiveBranch("the only live worker".to_string())),
                1 => {
                    let (id, worker) = workers.iter().next().expect("one entry");
                    Ok((id.clone(), Arc::clone(worker)))
                }
                n => Err(SteerError::Ambiguous(n)),
            },
        }
    }

    /// Steer the live branch selected by `node_id` (or the only live one) with
    /// `message`: run ONE more turn via [`WorkerSession::steer`], stream its events
    /// through `on_event`, record the turn (events + final result) into `ledger`
    /// under the resolved node id, and return the new [`TurnResult`].
    ///
    /// The looked-up node id is returned so the caller can scope its protocol
    /// events; a closed/one-shot/failed turn maps to a [`SteerError`].
    pub(crate) fn steer(
        &self,
        node_id: Option<&str>,
        message: &str,
        cancel: &CancelToken,
        ledger: &WorkerLedger,
        on_event: &mut dyn FnMut(&str, WorkerEvent),
    ) -> Result<(String, TurnResult), SteerError> {
        // Resolve the target node id + live handle (one lock pass) so events are
        // scoped correctly even for the unset "only live worker" selector.
        let (resolved, worker) = self.resolve(node_id)?;
        let mut guard = crate::sync::lock_recover(&worker.session);
        let session = guard.as_mut().ok_or(SteerError::NotSteerable)?;
        let mut events: Vec<WorkerEvent> = Vec::new();
        let mut sink = |event: WorkerEvent| {
            on_event(&resolved, event.clone());
            events.push(event);
        };
        let result = match session.steer(message, cancel, &mut sink) {
            Ok(result) => result,
            Err(WorkerError::NotSteerable) => return Err(SteerError::NotSteerable),
            Err(err) => return Err(SteerError::Turn(err.to_string())),
        };
        // Record the steered turn into the ledger (it is part of the replay tape).
        for event in events {
            ledger.record_event(&resolved, event);
        }
        ledger.record_result(&resolved, &result);
        Ok((resolved, result))
    }
}
