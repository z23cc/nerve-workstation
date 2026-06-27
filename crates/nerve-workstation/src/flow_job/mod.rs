//! Daemon execution of the `flow.*` command family (Wave C2).
//!
//! This is the host-layer composition that runs the deterministic C1 flow engine
//! ([`crate::flow`]) as a cancellable daemon **job** over the additive runtime
//! protocol (agent-orchestration design §4). It is the flow analogue of
//! [`crate::jobs`]'s `delegate.*` / `session.*` execution: a `flow.start` job runs
//! one [`Driver`](crate::flow::Driver) to completion in its job thread, mapping the
//! engine's worker events + node lifecycle onto the `flow_*` [`RuntimeEvent`]s; the
//! `job_id` IS the `flow_id`. `flow.get` / `flow.list` / `flow.close` mirror the
//! session/job equivalents through a live-flow registry, and `flow.respond` reuses
//! the existing [`ApprovalHub`] round-trip keyed by `flow_id` — exactly the
//! delegate/session pattern, no new approval mechanism.
//!
//! Determinism: the engine is pure; this module is the (nondeterministic) host
//! boundary that mints workers, streams events, and tracks live flows. It adds
//! NOTHING to `nerve-core`.

mod live;
mod observer;
mod persist;
mod replay;

pub(crate) use live::LiveFlows;
pub(crate) use replay::run_flow_replay;

use crate::flow::{Driver, FactoryResolver, FlowOutcome};
use crate::flow_store::{FlowRecord, FlowStore};
use crate::policy::{Policy, ToolGate};
use crate::providers::ProviderRegistry;
use crate::sandbox::SandboxLauncher;
use crate::session_manager::{ApprovalHub, FlowDecisionMemory, FlowProtocolApprover};
use crate::subagent::DEFAULT_MAX_DEPTH;
use crate::tools::NerveRuntime;
use crate::worker::{
    BudgetDecision, BudgetLedger, FleetBudget, SpawnRefusal, SteerError, SteerRegistry,
    WorkerEvent, WorkerFactory, WorkerLedger,
};
use nerve_core::{CancelToken, WorkspaceResolver};
use nerve_runtime::{
    ApprovalMode, FlowRunOutcome, FlowSource, RuntimeError, RuntimeEvent, SessionApprovalDecision,
    WorkerSelector, WorkflowDef,
};
use observer::{FlowEventObserver, emit_strategy_edges};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub(super) type EventEmitter = dyn Fn(RuntimeEvent) + Send + Sync + 'static;

/// The shared deps a flow needs to build its [`WorkerFactory`] + gate + approver.
/// Cloned from the [`JobManager`](crate::jobs)'s own deps so the flow engine reaches
/// tools through the SAME runtime and is gated by the SAME policy.
#[derive(Clone)]
pub(crate) struct FlowDeps {
    pub(crate) runtime: Arc<NerveRuntime>,
    pub(crate) registry: ProviderRegistry,
    pub(crate) policy: Policy,
    /// Trust-bound launcher for CLI workers (a refusing launcher unless the daemon
    /// was started with `--allow-delegate`, mirroring the delegate job path).
    pub(crate) delegate_launcher: Arc<dyn SandboxLauncher>,
    /// The shared approval hub `flow.respond` resolves against (the same hub
    /// `session.respond` / delegate proxying use).
    pub(crate) approvals: Arc<ApprovalHub>,
    /// Durable flow persistence under `.nerve/flows` (C4, design §5): a `flow.start`
    /// persists its def + ledger at node boundaries here, and `flow.replay` loads the
    /// recorded ledger from it. `None` disables persistence (no resolvable scope), so
    /// a flow still runs in-memory exactly as in C2/C3.
    pub(crate) store: Option<FlowStore>,
}

/// Execute a `flow.start`: register the flow, emit `flow_started`, run the C1
/// engine to completion mapping its lifecycle onto `flow_*` events, then emit
/// `flow_completed` (or `flow_failed`) and return turn-summary JSON as the job
/// result. The `flow_id` is the job id. `cancel` is the job's own token; it is
/// linked into the registry's token so a `flow.close` OR a `runtime/jobs/cancel`
/// both stop the engine.
pub(crate) fn run_flow_start(
    flow_id: &str,
    workflow: FlowSource,
    deps: &FlowDeps,
    flows: &LiveFlows,
    emit: &Arc<EventEmitter>,
    allow_delegate: bool,
    cancel: &CancelToken,
) -> Result<Value, RuntimeError> {
    let root = deps.workspace_root();
    // C6: worker-as-data + named-workflow discovery, scoped to the resolved project
    // root (`.nerve/{workers,workflows}` > global > built-in). A `workflow_ref`
    // resolves through the workflow registry; named workers resolve through the worker
    // registry the factory consults. The static cycle check walks resolved Named refs.
    let workers = crate::worker::WorkerRegistry::discover(root.as_deref());
    let workflows = crate::flow::WorkflowRegistry::discover(root.as_deref());
    let def = resolve_workflow(workflow, &workflows, &workers)?;
    // The shared ledger + live-flow worker registry: both are held by the registry
    // entry so a concurrent `flow.steer` reaches this flow's live frontier and
    // records its follow-up into the same tape (C3a).
    let ledger = Arc::new(WorkerLedger::new());
    let steer = Arc::new(SteerRegistry::new());
    // Per-flow budget governance (C3b, design §6/§8): the BudgetLedger is a pure fold
    // over each node's recorded usage (so it is replayable); the FleetBudget carves the
    // spawn-control envelope from the WorkflowDef's budget + max_depth. A default
    // (all-None) BudgetSpec caps nothing — the C2/C3a behaviour. Built BEFORE `register`
    // so the live-flow entry holds them and a concurrent `flow.steer` debits/refuses
    // against the SAME envelope as a driver-dispatched turn (finding C).
    let budget = Arc::new(BudgetLedger::new(def.budget));
    let fleet = FleetBudget::root(
        def.max_depth,
        def.budget.max_workers,
        budget.remaining_usd(),
        budget.remaining_tokens(),
    );
    let registry_cancel = flows.register(
        flow_id,
        &def,
        Arc::clone(&steer),
        Arc::clone(&ledger),
        Arc::clone(&budget),
        fleet.clone(),
    );
    // The engine runs under a token that fires if EITHER the job's own cancel OR a
    // flow.close (the registry token) fires.
    let run_cancel = combined_cancel(cancel, &registry_cancel);
    // Provider-worker gate: `Ask` resolves through the shared ApprovalHub keyed by
    // this flow id (so a gated tool raises ONE ApprovalRequested answered by
    // flow.respond), under the run token so a close aborts a pending approval.
    let gate = deps.gate_for_flow(flow_id, &run_cancel);
    let factory = build_factory(deps, gate, allow_delegate, workers);

    emit(RuntimeEvent::flow_started(flow_id, def.strategy.clone()));
    emit_strategy_edges(flow_id, &def.strategy, emit);

    // Durable persistence (C4, design §5): persist the def + an initial record at
    // start, then the observer persists the ledger at each node boundary. A store
    // error never fails the flow (persistence is durability, not correctness).
    let mut record = persist::persist_flow_start(deps.store.as_ref(), flow_id, &def);

    // CLI workers raise approvals through the same hub via `WorkerContext.approver`.
    let approver: Arc<dyn crate::delegate_proxy::DelegateApprover> =
        Arc::clone(&deps.approvals) as Arc<dyn crate::delegate_proxy::DelegateApprover>;
    let resolver = FactoryResolver::new(factory);
    let observer = {
        let observer = FlowEventObserver::new(flow_id.to_string(), Arc::clone(emit));
        match &deps.store {
            Some(store) => observer.with_persistence(store.clone(), Arc::clone(&ledger)),
            None => observer,
        }
    };

    // Per-node snapshot generation (C4, design §5): pin the live workspace snapshot
    // generation at each node-start and record it in the ledger, so a node that
    // mutated files makes a LATER node observe a different generation — replayed
    // honestly. Backed by the shared runtime's resolver; `0` when no snapshot resolves.
    let generation = persist::live_generation_provider(Arc::clone(&deps.runtime));
    // Writer-node path-leases (C4, design §6): a per-flow registry so two writer-nodes
    // on overlapping path scope SERIALIZE — safety + replay-fidelity precondition.
    let leases = crate::worker::PathLeases::new();

    // The progress sink maps each worker `Step` onto a node-scoped `FlowNodeAgent`;
    // the observer maps node start/finish onto `FlowNodeStarted`/`FlowNodeFinished`
    // and budget debits/refusals onto `BudgetUpdate`/`BudgetWarning`/`FlowDecision`.
    let on_progress = |progress: crate::flow::FlowProgress| observer.on_progress(&progress);
    let driver = Driver::new(&resolver, Arc::clone(&ledger), approver, root)
        .with_flow_id(flow_id)
        .with_observer(&observer)
        .with_progress(&on_progress)
        .with_steer_registry(&steer)
        .with_budget(Arc::clone(&budget), fleet)
        .with_generation(&generation)
        .with_leases(&leases);
    let outcome = driver.run(&def, &run_cancel);

    // Persist the terminal record + final tape (C4 node-boundary persistence's last
    // checkpoint), then emit the terminal protocol event + return the job result.
    persist::persist_flow_finish(deps.store.as_ref(), &mut record, &outcome, &ledger);
    finish_flow(
        flow_id,
        &def,
        &outcome,
        flows,
        emit,
        cancel,
        &registry_cancel,
    )
}

/// Resolve the additive [`FlowSource`] into a concrete [`WorkflowDef`], then run the
/// static safety checks (design §8). The inline form is used verbatim; a `workflow_ref`
/// is resolved through the C6 named-workflow [`WorkflowRegistry`](crate::flow::WorkflowRegistry)
/// (project `.nerve/workflows/*.json` > global > built-in). A workflow that fails the
/// reference-aware [`validate_workflow_refs`](crate::flow::validate_workflow_refs) (a
/// zero-depth hierarchy, an unresolvable named worker, a planner fork-loop, OR a
/// reference cycle through named workers / nested named workflow_refs) is REJECTED at
/// `flow.start`, before any worker spawns — the bounded-recursion model's front door.
fn resolve_workflow(
    workflow: FlowSource,
    workflows: &crate::flow::WorkflowRegistry,
    workers: &crate::worker::WorkerRegistry,
) -> Result<WorkflowDef, RuntimeError> {
    let def = match workflow {
        FlowSource::Inline { workflow } => *workflow,
        FlowSource::Named { workflow_ref } => workflows
            .resolve(&workflow_ref)
            .map_err(|err| RuntimeError::adapter(format!("workflow `{workflow_ref}`: {err}")))?,
    };
    crate::flow::validate_workflow_refs(&def, workflows, workers)
        .map_err(|err| RuntimeError::adapter(format!("invalid workflow: {err}")))?;
    Ok(def)
}

/// Build the [`WorkerFactory`] over the flow's deps + the flow-scoped `gate` + the C6
/// worker-as-data registry. CLI workers run under the trust-bound delegate launcher
/// (refusing unless `--allow-delegate`); provider workers reach tools through the
/// shared runtime behind `gate`. A registry-driven `remote`/`mcp` (exec-tier) worker
/// is opened only when `--allow-delegate` lifted the fleet (security before openness,
/// design §9) — and even then its production transport is the documented follow-on.
fn build_factory(
    deps: &FlowDeps,
    gate: ToolGate,
    allow_delegate: bool,
    workers: crate::worker::WorkerRegistry,
) -> WorkerFactory {
    let delegate_launcher = if allow_delegate {
        Arc::clone(&deps.delegate_launcher)
    } else {
        crate::sandbox::refuse_launcher()
    };
    let factory = WorkerFactory::new(
        delegate_launcher,
        Arc::clone(&deps.runtime),
        deps.registry.clone(),
        gate,
        DEFAULT_MAX_DEPTH,
    )
    .with_registry(workers);
    if allow_delegate {
        // The fleet is explicitly opened: exec-tier remote/MCP workers may be minted
        // (the gate is the same lift a CLI worker passes). The production transport is
        // a documented follow-on — the connector returns a clear not-yet-wired error.
        factory.with_remote(Arc::new(crate::flow_remote::FollowOnConnector))
    } else {
        factory
    }
}

impl FlowDeps {
    /// The workspace root the flow is scoped to (the first registered root), used to
    /// discover `.nerve/{workers,workflows}` defs (C6) and locate `.nerve/flows`.
    /// `None` when no root resolves (built-ins still resolve).
    fn workspace_root(&self) -> Option<std::path::PathBuf> {
        self.runtime
            .resolver()
            .resolve_workspace(None)
            .ok()
            .and_then(|ws| ws.roots().first().map(|r| r.path.clone()))
    }

    /// Build a provider-worker gate whose `Ask` decisions route through the shared
    /// [`ApprovalHub`] keyed by `flow_id`, with per-flow decision memory.
    fn gate_for_flow(&self, flow_id: &str, cancel: &CancelToken) -> ToolGate {
        let memory: FlowDecisionMemory = Arc::new(Mutex::new(HashMap::new()));
        let approver = Arc::new(FlowProtocolApprover::new(
            flow_id.to_string(),
            Arc::clone(&self.approvals),
            cancel.clone(),
            memory,
        ));
        ToolGate::with_approver(self.policy.clone(), ApprovalMode::AlwaysAsk, approver)
    }
}

/// Execute a `flow.steer` (C3a): look up the live flow's worker registry, run one
/// more turn against the branch `target` selects (via the C0 [`WorkerSession::steer`]
/// port), stream the follow-up turn as node-scoped [`RuntimeEvent::FlowNodeAgent`]
/// events, and record it into the flow's ledger. A finished flow, a missing/closed
/// branch, an ambiguous unset selector, or a one-shot worker (a remote/MCP worker)
/// errors cleanly — no live LLM/subprocess is touched here beyond the existing session.
pub(crate) fn run_flow_steer(
    flow_id: &str,
    target: &WorkerSelector,
    message: &str,
    flows: &LiveFlows,
    emit: &Arc<EventEmitter>,
    cancel: &CancelToken,
) -> Result<Value, RuntimeError> {
    let live::SteerTarget {
        steer,
        ledger,
        budget,
        fleet,
        cancel: run_cancel,
    } = flows.steer_target(flow_id)?;
    // Budget envelope (finding C): a steered turn must honor the SAME envelope as a
    // driver-dispatched one. Refuse before running if the budget is already exhausted,
    // then acquire a process-global worker slot (refusing at the worker ceiling). The
    // slot is held for the steer turn's lifetime and released on drop.
    if budget.is_exhausted() {
        return Err(RuntimeError::adapter(format!(
            "flow `{flow_id}` steer refused: fleet budget exhausted"
        )));
    }
    let _slot = fleet
        .acquire()
        .map_err(|refusal| RuntimeError::adapter(steer_refusal_message(flow_id, refusal)))?;
    let flow = flow_id.to_string();
    let emit = Arc::clone(emit);
    // Each steered event becomes a node-scoped FlowNodeAgent (the same projection
    // the driver's progress sink uses for turn 1), keyed by the resolved node id.
    let mut on_event = |node: &str, event: WorkerEvent| {
        if let WorkerEvent::Step(kind) = event {
            emit(RuntimeEvent::flow_node_agent(
                flow.clone(),
                node.to_string(),
                kind,
            ));
        }
    };
    match steer.steer(
        target.node_id.as_deref(),
        message,
        cancel,
        &ledger,
        &mut on_event,
    ) {
        Ok((node, result)) => {
            // Debit the steered turn's Usage into the SAME budget fold as a
            // driver-dispatched turn (finding C). On exhaustion, cooperatively cancel
            // the flow's run token so the driver's remaining turns stop too — exactly
            // the `BudgetDecision::Exhausted` brake the driver applies.
            if matches!(budget.debit(&result), BudgetDecision::Exhausted) {
                run_cancel.cancel();
            }
            Ok(json!({
                "flow_id": flow_id,
                "node_id": node,
                "ok": result.ok,
                "text": result.text,
                "steered": true,
            }))
        }
        Err(err) => Err(steer_error(flow_id, err)),
    }
}

/// A clear message for a `flow.steer` refused at the spawn-control ceiling (finding C):
/// the worker semaphore is full, the depth ceiling is hit, or the budget is dry. Mirrors
/// the driver's recorded-refusal vocabulary so an operator sees the same reason.
fn steer_refusal_message(flow_id: &str, refusal: SpawnRefusal) -> String {
    let reason = match refusal {
        SpawnRefusal::Depth { depth, max_depth } => {
            format!("depth ceiling ({depth}/{max_depth})")
        }
        SpawnRefusal::Workers {
            live_workers,
            max_workers,
        } => format!("worker ceiling ({live_workers}/{max_workers})"),
        SpawnRefusal::Budget => "fleet budget exhausted".to_string(),
    };
    format!("flow `{flow_id}` steer refused: {reason}")
}

/// Map a [`SteerError`] onto a runtime error. A turn cancellation surfaces as a
/// cancelled error (so the `flow.steer` job finishes `job_cancelled`); every other
/// case is a clear adapter error the client renders.
fn steer_error(flow_id: &str, err: SteerError) -> RuntimeError {
    match err {
        SteerError::Turn(message) if message.contains("cancel") => RuntimeError::cancelled(),
        other => RuntimeError::adapter(format!("flow `{flow_id}` steer failed: {other}")),
    }
}

/// Resolve a `flow.get`. Mirrors `session.get` / `runtime/jobs/get`. Falls back to the
/// [`FlowStore`] (C4) for a flow that is no longer live in memory but was persisted —
/// so a flow stays INSPECTABLE after the daemon (re)started.
pub(crate) fn run_flow_get(
    flow_id: &str,
    flows: &LiveFlows,
    store: Option<&FlowStore>,
) -> Result<Value, RuntimeError> {
    if let Ok(flow) = flows.get(flow_id) {
        return Ok(json!({ "flow": flow }));
    }
    let store = store.ok_or_else(|| RuntimeError::adapter(format!("no flow `{flow_id}`")))?;
    let record = store
        .load_record(flow_id)
        .map_err(|err| RuntimeError::adapter(format!("no flow `{flow_id}`: {err}")))?;
    Ok(json!({ "flow": persisted_flow_snapshot(&record) }))
}

/// Resolve a `flow.list`. Mirrors `session.list`. Merges live flows with persisted
/// ones from the [`FlowStore`] (C4), de-duplicating by id (a live flow shadows its
/// persisted record), so a client sees both running and past flows.
pub(crate) fn run_flow_list(
    flows: &LiveFlows,
    store: Option<&FlowStore>,
) -> Result<Value, RuntimeError> {
    let live = flows.list();
    let mut entries: Vec<Value> = live
        .get("flows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(store) = store
        && let Ok(records) = store.list()
    {
        let live_ids = live_flow_ids(&entries);
        for record in records {
            if !live_ids.contains(&record.flow_id) {
                entries.push(persisted_flow_snapshot(&record));
            }
        }
    }
    Ok(json!({ "flows": entries }))
}

/// The set of flow ids already present in the live-flow list (so a persisted record
/// for a still-live flow is not listed twice).
fn live_flow_ids(entries: &[Value]) -> std::collections::HashSet<String> {
    entries
        .iter()
        .filter_map(|e| e.get("flow_id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// Project a persisted [`FlowRecord`] onto the same JSON shape a live flow snapshot
/// uses (`flow_id` / `name` / `strategy` / `status` / `outcome`), so a client renders
/// persisted and live flows uniformly. A persisted flow's status is always `finished`
/// (only finished or last-checkpointed flows survive on disk after a restart).
fn persisted_flow_snapshot(record: &FlowRecord) -> Value {
    let outcome = record
        .outcome
        .as_ref()
        .map(|o| json!({ "ok": o.ok, "summary": o.summary, "final_text": o.final_text }));
    json!({
        "flow_id": record.flow_id,
        "name": record.name,
        "strategy": record.strategy,
        "status": if record.finished { "finished" } else { "interrupted" },
        "outcome": outcome,
        "persisted": true,
    })
}

/// Resolve a `flow.close`. Mirrors `session.close` / `delegate.close`.
pub(crate) fn run_flow_close(flow_id: &str, flows: &LiveFlows) -> Result<Value, RuntimeError> {
    flows.close(flow_id)
}

/// Resolve a `flow.respond`: route the decision through the shared [`ApprovalHub`]
/// keyed by `flow_id` (a flow branch is just another approval id). Reuses the SAME
/// round-trip `session.respond` / delegate approvals use — no new mechanism.
pub(crate) fn run_flow_respond(
    flow_id: &str,
    request_id: &str,
    decision: SessionApprovalDecision,
    approvals: &ApprovalHub,
) -> Result<Value, RuntimeError> {
    let responded = approvals.respond(flow_id, request_id, decision);
    Ok(json!({ "flow_id": flow_id, "request_id": request_id, "responded": responded }))
}

/// Emit the terminal `flow_completed` / `flow_failed` event, record the outcome in
/// the registry, and return the job-result JSON. A cancelled flow (the job token or
/// a `flow.close` fired) maps to a cancelled [`RuntimeError`] so the job finishes
/// `job_cancelled`, not `job_failed`.
pub(super) fn finish_flow(
    flow_id: &str,
    def: &WorkflowDef,
    outcome: &FlowOutcome,
    flows: &LiveFlows,
    emit: &Arc<EventEmitter>,
    job_cancel: &CancelToken,
    registry_cancel: &CancelToken,
) -> Result<Value, RuntimeError> {
    // Read the cancellation status BEFORE we trip the registry token below.
    let cancelled = job_cancel.is_cancelled() || registry_cancel.is_cancelled();
    // Terminal cleanup (finding E): a normally-completing flow never fires either the
    // job or the registry cancel, so the `combined_cancel` watcher thread — which only
    // exits when one of those is cancelled — would spin/park forever, leaking one OS
    // thread per completed flow/replay. Trip the registry token here on EVERY terminal
    // path so the watcher observes `is_cancelled()` and returns. Safe: the engine has
    // already returned its outcome, and we captured `cancelled` above.
    registry_cancel.cancel();
    let run_outcome = FlowRunOutcome {
        ok: outcome.ok,
        summary: outcome.summary.clone(),
        final_text: outcome.final_text(),
    };
    flows.record_outcome(flow_id, run_outcome.clone());
    if cancelled {
        emit(RuntimeEvent::flow_failed(
            flow_id,
            None,
            "flow cancelled".to_string(),
        ));
        return Err(RuntimeError::cancelled());
    }
    if outcome.ok {
        emit(RuntimeEvent::flow_completed(flow_id, run_outcome.clone()));
    } else {
        emit(RuntimeEvent::flow_failed(
            flow_id,
            None,
            outcome.summary.clone(),
        ));
    }
    Ok(json!({
        "flow_id": flow_id,
        "name": def.name,
        "ok": outcome.ok,
        "summary": outcome.summary,
        "final_text": run_outcome.final_text,
    }))
}

/// Combine the job's own cancel token and the registry's flow-close token into one
/// token the engine polls, so either source stops the flow. A tiny watcher fans
/// both into the combined token (the engine loop only checks `is_cancelled()`).
pub(super) fn combined_cancel(job: &CancelToken, registry: &CancelToken) -> CancelToken {
    let combined = CancelToken::new();
    if job.is_cancelled() || registry.is_cancelled() {
        combined.cancel();
        return combined;
    }
    let out = combined.clone();
    let job = job.clone();
    let registry = registry.clone();
    std::thread::spawn(move || {
        loop {
            if job.is_cancelled() || registry.is_cancelled() {
                out.cancel();
                return;
            }
            if out.is_cancelled() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });
    combined
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::FlowOutcome;
    use crate::worker::{TurnResult, WorkerSession};
    use nerve_runtime::{BudgetSpec, Step, Strategy, TaskTemplate, WorkerRef, WorkerSelector};

    /// A steerable test session: each steer returns a fixed-usage [`TurnResult`] so a
    /// test can assert the steer path debited the flow's budget (finding C).
    struct UsageSession {
        result: TurnResult,
    }

    impl WorkerSession for UsageSession {
        fn steer(
            &mut self,
            _message: &str,
            _cancel: &CancelToken,
            on_event: &mut dyn FnMut(WorkerEvent),
        ) -> Result<TurnResult, crate::worker::WorkerError> {
            crate::worker::synthesize_turn_steps(2, &self.result, on_event);
            Ok(self.result.clone())
        }
        fn interrupt(&self) {}
        fn close(&mut self) {}
        fn result(&self) -> TurnResult {
            self.result.clone()
        }
    }

    /// A pricey turn result: $0.50 + 1000 tokens (so a couple exceed a small cap).
    fn pricey_turn(text: &str) -> TurnResult {
        TurnResult {
            ok: true,
            text: text.into(),
            usage: nerve_agent::Usage {
                input_tokens: 600,
                output_tokens: 400,
                ..nerve_agent::Usage::default()
            },
            cost_usd: Some(0.50),
            timed_out: false,
        }
    }

    /// Register a flow with one steerable frontier under `node`, plus a budget over
    /// `spec` (a `flow.steer` debits/refuses against it). Returns the registry handle.
    fn register_steerable_flow(
        flows: &LiveFlows,
        flow_id: &str,
        node: &str,
        spec: BudgetSpec,
    ) -> Arc<BudgetLedger> {
        let def = tiny_def();
        let steer = Arc::new(SteerRegistry::new());
        steer.register(
            node,
            Box::new(UsageSession {
                result: pricey_turn("turn 1"),
            }),
        );
        let budget = Arc::new(BudgetLedger::new(spec));
        let fleet = FleetBudget::root(
            def.max_depth,
            spec.max_workers,
            budget.remaining_usd(),
            budget.remaining_tokens(),
        );
        flows.register(
            flow_id,
            &def,
            steer,
            Arc::new(WorkerLedger::new()),
            Arc::clone(&budget),
            fleet,
        );
        budget
    }

    #[test]
    fn steer_debits_its_usage_into_the_flow_budget() {
        // Finding C: a steered turn must be debited from the SAME BudgetLedger as a
        // driver-dispatched turn. Steer a flow under a generous budget and assert the
        // ledger's spend rose by the steered turn's cost/tokens.
        let flows = LiveFlows::default();
        let budget = register_steerable_flow(
            &flows,
            "flow-steer",
            "node-0",
            BudgetSpec {
                max_total_cost_usd: Some(100.0),
                max_total_tokens: Some(1_000_000),
                max_workers: None,
            },
        );
        assert_eq!(budget.snapshot().spent_usd, 0.0, "nothing spent yet");

        let emit: Arc<EventEmitter> = Arc::new(|_event| {});
        let result = run_flow_steer(
            "flow-steer",
            &WorkerSelector::node("node-0"),
            "keep going",
            &flows,
            &emit,
            &CancelToken::never(),
        );
        assert!(result.is_ok(), "the steer ran: {result:?}");
        let snap = budget.snapshot();
        assert!(
            (snap.spent_usd - 0.50).abs() < 1e-9,
            "the steered turn's USD cost was debited (got {})",
            snap.spent_usd
        );
        assert_eq!(
            snap.spent_tokens, 1000,
            "the steered turn's tokens were debited"
        );
    }

    #[test]
    fn steer_on_an_exhausted_budget_is_refused() {
        // Finding C: steering a flow whose budget is already exhausted must be REFUSED
        // (it used to bypass the envelope entirely). Seed the budget over its cap, then
        // assert `run_flow_steer` errors rather than running the turn.
        let flows = LiveFlows::default();
        let budget = register_steerable_flow(
            &flows,
            "flow-broke",
            "node-0",
            BudgetSpec {
                max_total_cost_usd: Some(1.0),
                max_total_tokens: None,
                max_workers: None,
            },
        );
        // Drive the budget over its $1.00 cap (two $0.50 debits -> exhausted on the
        // boundary-crossing debit; a third makes `is_exhausted` unambiguous).
        budget.debit(&pricey_turn("a"));
        budget.debit(&pricey_turn("b"));
        budget.debit(&pricey_turn("c"));
        assert!(budget.is_exhausted(), "the budget is over its cap");

        let emit: Arc<EventEmitter> = Arc::new(|_event| {});
        let result = run_flow_steer(
            "flow-broke",
            &WorkerSelector::node("node-0"),
            "keep going",
            &flows,
            &emit,
            &CancelToken::never(),
        );
        let err = result.expect_err("steering an exhausted flow must be refused");
        assert!(
            err.to_string().contains("exhausted"),
            "the refusal names the exhausted budget: {err}"
        );
        // And the refused steer did NOT run a turn (no further spend beyond the seed).
        assert_eq!(
            budget.snapshot().spent_usd,
            1.5,
            "a refused steer never debits a new turn"
        );
    }

    fn tiny_def() -> WorkflowDef {
        WorkflowDef {
            schema_version: 1,
            name: "leak-test".into(),
            strategy: Strategy::Single {
                step: Step {
                    worker: WorkerRef::Cli {
                        name: "claude".into(),
                    },
                    task: TaskTemplate::new("noop"),
                    autonomy: nerve_runtime::DelegateAutonomy::ReadOnly,
                    on_fail: nerve_runtime::FailPolicy::Continue,
                },
            },
            budget: BudgetSpec::default(),
            max_depth: 2,
        }
    }

    #[test]
    fn finish_flow_on_completion_stops_the_combined_cancel_watcher() {
        // Finding E: a normally-completing flow must NOT leak the `combined_cancel`
        // watcher thread. `finish_flow` trips the registry token on completion, so the
        // watcher observes `is_cancelled()`, fans it into the combined token, and exits.
        // We assert the watcher observed it by polling the COMBINED token to cancel
        // (which only the watcher does, on its way to returning).
        let flows = LiveFlows::default();
        let def = tiny_def();
        let budget = Arc::new(BudgetLedger::new(def.budget));
        let fleet = FleetBudget::root(def.max_depth, def.budget.max_workers, None, None);
        let registry_cancel = flows.register(
            "flow-leak",
            &def,
            Arc::new(SteerRegistry::new()),
            Arc::new(WorkerLedger::new()),
            budget,
            fleet,
        );
        let job_cancel = CancelToken::new();
        // The watcher thread is spawned here, watching job + registry tokens.
        let combined = combined_cancel(&job_cancel, &registry_cancel);
        assert!(!combined.is_cancelled(), "the flow has not been cancelled");

        let outcome = FlowOutcome {
            ok: true,
            results: Vec::new(),
            summary: "done".into(),
        };
        let emit: Arc<EventEmitter> = Arc::new(|_event| {});
        let result = finish_flow(
            "flow-leak",
            &def,
            &outcome,
            &flows,
            &emit,
            &job_cancel,
            &registry_cancel,
        );
        // A completed (not job/registry-cancelled-before-finish) flow returns ok.
        assert!(
            result.is_ok(),
            "a completed flow returns its result, not cancelled"
        );
        assert!(
            registry_cancel.is_cancelled(),
            "finish_flow trips the registry token so the watcher can observe it"
        );
        // The watcher polls every 20ms; within a generous window it must observe the
        // registry cancel and fan it into the combined token, then RETURN (no leak).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !combined.is_cancelled() {
            assert!(
                std::time::Instant::now() < deadline,
                "the watcher never observed the completion — it leaked"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
