//! The production [`ReplayWorker`] — REPLAY mode (design §3).
//!
//! A [`ReplayWorker`] re-emits a RECORDED node's events and result instead of
//! calling any LLM/subprocess: it reads a recorded [`WorkerLedger`] tape and, for
//! the node whose stable [`NodeId`](crate::flow::NodeId) it is handed, replays that
//! node's `Event` entries (in recorded seq order) and recovers its `Result`. This is
//! the engine running offline over a tape — the audit moat (`flow.replay`, the
//! byte-identical CI gate).
//!
//! ## Why keying by node id is faithful (and why the rendered prompt is NOT)
//!
//! Replay runs the SAME deterministic engine over the SAME [`WorkflowDef`], which
//! assigns each dispatch the SAME stable `NodeId` it did at record time (the id is a
//! pure function of the strategy shape + position, never of the prompt). So the node
//! a replay worker is handed (`task.node_id`) matches the recorded one exactly, and
//! its recorded tape is recovered directly. Keying by the RENDERED PROMPT instead is
//! UNSOUND: two distinct nodes can legally render an identical prompt (a `Parallel`
//! with two identical branch templates; a `MapReduce` whose `map` step omits
//! `{{split}}`), which collides — one node's events would be re-emitted for both,
//! breaking byte-identical replay and corrupting the blackboard. The prompt is kept
//! on the `Start` entry as an advisory record only.

use super::{
    AgentWorker, LedgerEntry, LedgerPayload, TurnResult, WorkerContext, WorkerError, WorkerEvent,
    WorkerKind, WorkerSession,
};
use nerve_core::CancelToken;
use nerve_runtime::RiskTier;
use std::sync::Arc;

/// A worker that re-emits a recorded node's tape (design §3, REPLAY). Keyed by the
/// stable [`NodeId`](crate::flow::NodeId) (`task.node_id`), so each replayed node
/// re-emits exactly its own recorded events and returns its recorded result — never
/// calling an LLM/subprocess, and never colliding with a sibling that renders the
/// same prompt.
pub(crate) struct ReplayWorker {
    /// The recorded tape (immutable; shared across all replay workers in a run).
    recorded: Arc<Vec<LedgerEntry>>,
}

impl ReplayWorker {
    /// Build a replay worker over a shared recorded tape.
    pub(crate) fn new(recorded: Arc<Vec<LedgerEntry>>) -> Self {
        Self { recorded }
    }
}

impl AgentWorker for ReplayWorker {
    fn kind(&self) -> WorkerKind {
        // Replay is worker-kind-agnostic; the recorded events already carry the
        // worker's behaviour. A stable label keeps the kind deterministic.
        WorkerKind::Cli("replay")
    }

    fn capability(&self) -> RiskTier {
        // A replay worker performs no live action, so it can reach nothing.
        RiskTier::ReadOnly
    }

    fn start(
        &self,
        task: &super::WorkerTask,
        _ctx: &WorkerContext,
        _cancel: &CancelToken,
        on_event: &mut dyn FnMut(WorkerEvent),
    ) -> Result<Box<dyn WorkerSession>, WorkerError> {
        let node = task.node_id.as_str();
        // A node id the recorded tape never started is a corrupt/mismatched tape, not
        // a normal outcome — fail closed rather than silently replaying nothing.
        if !self.recorded.iter().any(|e| e.node_id == node) {
            return Err(WorkerError::Start(format!(
                "no recorded node `{node}` in the replay tape"
            )));
        }
        // Re-emit this node's recorded events, in recorded seq order, and recover its
        // recorded final result — never touching an LLM/process. The `Start` entry is
        // metadata (prompt + generation), not re-emitted.
        let mut last = TurnResult {
            ok: false,
            text: "replay: node had no recorded result".into(),
            usage: nerve_agent::Usage::default(),
            cost_usd: None,
            timed_out: false,
        };
        for entry in self.recorded.iter().filter(|e| e.node_id == node) {
            match &entry.payload {
                LedgerPayload::Event(event) => on_event(event.clone()),
                LedgerPayload::Result(result) => last = result.clone(),
                LedgerPayload::Start { .. } => {}
            }
        }
        Ok(Box::new(ReplaySession { last }))
    }
}

/// A replayed session: turn 1 already re-emitted in `start`. Steering is refused —
/// a replay re-runs only the recorded turns (recorded nondeterminism, §5), it never
/// invents a new one.
struct ReplaySession {
    last: TurnResult,
}

impl WorkerSession for ReplaySession {
    fn steer(
        &mut self,
        _message: &str,
        _cancel: &CancelToken,
        _on_event: &mut dyn FnMut(WorkerEvent),
    ) -> Result<TurnResult, WorkerError> {
        Err(WorkerError::NotSteerable)
    }
    fn interrupt(&self) {}
    fn close(&mut self) {}
    fn result(&self) -> TurnResult {
        self.last.clone()
    }
}
