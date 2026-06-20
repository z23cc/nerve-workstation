//! The live-flow registry — the daemon's view of running + finished flows (C2/C3a).
//!
//! Split out of [`flow_job`](super) for the file-size convention. [`LiveFlows`] is a
//! sibling of [`LiveSessions`](crate::delegate_live::LiveSessions): it tracks each
//! flow's cancel token (so `flow.close` / job-cancel can interrupt the engine loop),
//! the live-flow worker registry + shared ledger a concurrent `flow.steer` reaches
//! the current frontier through (C3a), and a lightweight status snapshot for
//! `flow.get` / `flow.list`. A flow runs synchronously in its job thread (no parking),
//! so the registry only needs the cancel handle + status, not a live driver.

use super::observer::strategy_label;
use crate::worker::{BudgetLedger, FleetBudget, SteerRegistry, WorkerLedger};
use nerve_core::CancelToken;
use nerve_runtime::{FlowRunOutcome, RuntimeError, WorkflowDef};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// What a `flow.steer` needs from the live-flow registry: the steerable frontier, the
/// shared ledger to record the follow-up turn into, the budget fold to debit it
/// against, the spawn-control envelope to refuse it at a ceiling, and the flow's run
/// cancel token to trip on budget exhaustion (finding C).
pub(super) struct SteerTarget {
    pub(super) steer: Arc<SteerRegistry>,
    pub(super) ledger: Arc<WorkerLedger>,
    pub(super) budget: Arc<BudgetLedger>,
    pub(super) fleet: FleetBudget,
    pub(super) cancel: CancelToken,
}

/// One live flow's registry entry: its cancel token (so `flow.close` can interrupt
/// the engine loop), the live-flow worker registry + shared ledger that
/// `flow.steer` reaches the current frontier through (C3a), plus a lightweight
/// status snapshot for `flow.get` / `flow.list`.
struct FlowEntry {
    cancel: CancelToken,
    strategy_label: &'static str,
    name: String,
    created_seq: u64,
    /// The live-flow worker registry the driver registers each steerable frontier
    /// into; `flow.steer` looks up the live worker here. Shared (`Arc`) with the
    /// run thread's [`Driver`](crate::flow::Driver).
    steer: Arc<SteerRegistry>,
    /// The flow's shared append-only ledger — a steered turn records into the SAME
    /// tape as the original turn (recorded nondeterminism, design §5).
    ledger: Arc<WorkerLedger>,
    /// The flow's budget fold (design §6). Shared with the run thread's [`Driver`] so a
    /// concurrent `flow.steer` debits its turn against the SAME ledger as a
    /// driver-dispatched turn — a steered turn can never escape the budget (finding C).
    budget: Arc<BudgetLedger>,
    /// The flow's spawn-control envelope (design §8). Shares the process-global worker
    /// semaphore with the driver, so a steered turn acquires a slot + is refused at the
    /// worker/budget ceiling exactly like a driver-dispatched turn (finding C).
    fleet: FleetBudget,
    /// Set once the flow finishes (the run thread records its terminal outcome).
    outcome: Option<FlowRunOutcome>,
}

/// The registry of flows the daemon knows about, keyed by `flow_id`. Live flows
/// carry an uncancelled token; finished flows retain a snapshot for `flow.get`
/// until pruned.
#[derive(Default)]
pub(crate) struct LiveFlows {
    flows: Mutex<HashMap<String, FlowEntry>>,
    next_seq: Mutex<u64>,
}

impl LiveFlows {
    /// Register a starting flow under `flow_id`, returning the cancel token the run
    /// thread drives the engine under (and `flow.close` fires). The `steer` registry,
    /// `ledger`, `budget`, and `fleet` are shared with the run thread's driver so a
    /// concurrent `flow.steer` reaches the live frontier + records into the same tape
    /// (C3a) AND debits/refuses against the SAME budget envelope (finding C).
    pub(super) fn register(
        &self,
        flow_id: &str,
        def: &WorkflowDef,
        steer: Arc<SteerRegistry>,
        ledger: Arc<WorkerLedger>,
        budget: Arc<BudgetLedger>,
        fleet: FleetBudget,
    ) -> CancelToken {
        let cancel = CancelToken::new();
        let created_seq = {
            let mut seq = crate::sync::lock_recover(&self.next_seq);
            *seq += 1;
            *seq
        };
        crate::sync::lock_recover(&self.flows).insert(
            flow_id.to_string(),
            FlowEntry {
                cancel: cancel.clone(),
                strategy_label: strategy_label(&def.strategy),
                name: def.name.clone(),
                created_seq,
                steer,
                ledger,
                budget,
                fleet,
                outcome: None,
            },
        );
        cancel
    }

    /// Look up a live flow's `(steer registry, ledger, budget, fleet)` for a
    /// `flow.steer`. Errors on an unknown id, or on a flow that has already finished
    /// (no live frontier). The budget + fleet are returned so the steer path debits +
    /// refuses against the flow's live envelope (finding C).
    pub(super) fn steer_target(&self, flow_id: &str) -> Result<SteerTarget, RuntimeError> {
        let flows = crate::sync::lock_recover(&self.flows);
        let entry = flows
            .get(flow_id)
            .ok_or_else(|| RuntimeError::adapter(format!("no flow `{flow_id}`")))?;
        if entry.outcome.is_some() {
            return Err(RuntimeError::adapter(format!(
                "flow `{flow_id}` has finished; nothing to steer"
            )));
        }
        Ok(SteerTarget {
            steer: Arc::clone(&entry.steer),
            ledger: Arc::clone(&entry.ledger),
            budget: Arc::clone(&entry.budget),
            fleet: entry.fleet.clone(),
            cancel: entry.cancel.clone(),
        })
    }

    /// Record a flow's terminal outcome (the run thread calls this when the driver
    /// returns), so a later `flow.get` reflects the result.
    pub(super) fn record_outcome(&self, flow_id: &str, outcome: FlowRunOutcome) {
        if let Some(entry) = crate::sync::lock_recover(&self.flows).get_mut(flow_id) {
            entry.outcome = Some(outcome);
        }
    }

    /// Snapshot one flow as JSON for `flow.get` (running vs. finished + its outcome).
    pub(super) fn get(&self, flow_id: &str) -> Result<Value, RuntimeError> {
        crate::sync::lock_recover(&self.flows)
            .get(flow_id)
            .map(|entry| entry.snapshot(flow_id))
            .ok_or_else(|| RuntimeError::adapter(format!("no flow `{flow_id}`")))
    }

    /// List all known flows as JSON, in registration order, for `flow.list`.
    pub(super) fn list(&self) -> Value {
        let flows = crate::sync::lock_recover(&self.flows);
        let mut entries: Vec<(&String, &FlowEntry)> = flows.iter().collect();
        entries.sort_by_key(|(_, entry)| entry.created_seq);
        let flows: Vec<Value> = entries
            .into_iter()
            .map(|(id, entry)| entry.snapshot(id))
            .collect();
        json!({ "flows": flows })
    }

    /// Request close of a live flow: fire its cancel token (the engine loop checks
    /// it each step and returns a cancelled outcome). Errors on an unknown id.
    pub(super) fn close(&self, flow_id: &str) -> Result<Value, RuntimeError> {
        let flows = crate::sync::lock_recover(&self.flows);
        let entry = flows
            .get(flow_id)
            .ok_or_else(|| RuntimeError::adapter(format!("no flow `{flow_id}`")))?;
        entry.cancel.cancel();
        Ok(json!({ "flow_id": flow_id, "closed": true }))
    }
}

impl FlowEntry {
    fn snapshot(&self, flow_id: &str) -> Value {
        let status = if self.outcome.is_some() {
            "finished"
        } else {
            "running"
        };
        json!({
            "flow_id": flow_id,
            "name": self.name,
            "strategy": self.strategy_label,
            "status": status,
            "outcome": self.outcome,
        })
    }
}
