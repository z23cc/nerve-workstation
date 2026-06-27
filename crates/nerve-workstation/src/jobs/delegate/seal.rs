//! L0 run-capture seal + live tool-row emission for delegated runs.
//!
//! Once a live delegated session finishes, its captured tape is sealed into one
//! content-addressed [`Run`](crate::run_store) and announced with `RunRecorded`; each
//! stream line's structured tool calls are ALSO lifted into live `DelegateAgent`
//! per-tool rows (Wave 3). Used by the live ([`JobManager::run_delegate_live`])
//! capture path.

use crate::delegate_runtime::{self, DelegateAgent};
use crate::jobs::{EventEmitter, JobManager};
use nerve_runtime::RuntimeEvent;
use serde_json::Value;
use std::sync::Arc;

impl JobManager {
    /// Seal a live delegated session's captured turns (claude/codex) into one
    /// content-addressed Run and announce it (best-effort). Each [`CapturedTurn`]
    /// becomes `TurnStarted` → the turn's verbatim raw-output `Output` lines →
    /// optional `UsageUpdated` → `TurnFinished`, bracketed by `RunStarted` /
    /// `RunFinished` — the full raw tape. `finished` is false when the session ended
    /// by cancellation.
    #[allow(clippy::too_many_arguments)] // reason: one cohesive seal call; the run
    // identity (job_id/agent), the start context (task/cwd/started_at_ms), the
    // captured turns, and the finished flag are independent inputs, and bundling them
    // into a struct would add indirection without isolating a separate responsibility
    // (mirrors `run_delegate_live` / `start_live_driver` in this file).
    pub(super) fn seal_live_run(
        &self,
        job_id: &str,
        agent: &str,
        task: &str,
        cwd: &std::path::Path,
        started_at_ms: u64,
        turns: Vec<crate::delegate_live::CapturedTurn>,
        finished: bool,
    ) {
        use nerve_core::provenance::EventKind;
        let resolved = DelegateAgent::from_name(agent).ok();
        let root = self.delegate_root().ok().map(|p| p.display().to_string());
        let mut writer = crate::run_store::RunWriter::begin_at(started_at_ms, job_id, agent, root);
        writer.push(EventKind::RunStarted {
            agent: agent.to_string(),
            task: task.to_string(),
            cwd: Some(cwd.display().to_string()),
            // L0c: pin the run's executed closure (repo snapshot + toolchain digest)
            // in-band so its content address commits to *what ran*, not just output.
            inputs: Some(crate::toolchain_pin::resolve_run_inputs(
                self.delegate_root().ok().as_deref(),
            )),
        });
        let mut last_ok = true;
        for (index, captured) in turns.iter().enumerate() {
            let turn = index as u64;
            writer.push(EventKind::TurnStarted { turn });
            // The verbatim raw tape this turn produced (the live-path analogue of the
            // one-shot path's per-line Output events) — interleaved per turn.
            for line in &captured.raw_lines {
                writer.push(EventKind::Output {
                    turn,
                    text: line.clone(),
                });
                // L0 granularity: index this turn's tool calls in tape order right
                // after their raw Output (unknown agent -> empty).
                if let Some(resolved) = resolved
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(line)
                {
                    for kind in delegate_runtime::parse_tool_events(resolved, &value, turn) {
                        writer.push(kind);
                    }
                }
            }
            if let Some(usage) = &captured.usage {
                writer.push(delegate_usage_event(turn, usage, captured.cost_usd));
            }
            writer.push(EventKind::TurnFinished {
                turn,
                ok: captured.ok,
            });
            last_ok = captured.ok;
        }
        writer.push(EventKind::RunFinished {
            ok: finished && last_ok,
            exit_code: None,
            timed_out: false,
        });
        self.emit_run_recorded(job_id, writer.seal(finished, self.run_store().as_ref()));
    }

    /// Emit a `RunRecorded` announcement for a sealed run, if it persisted, AND append
    /// a `RunRecorded` evidence record to the L1 cross-run ledger (announcing each
    /// append via `LedgerAppended`). Both are best-effort no-ops when sealing was
    /// skipped (no served root / write failure) — capture/ledger is an audit seam,
    /// never a gate on the delegated turn (INV-R1/INV-R2).
    fn emit_run_recorded(&self, job_id: &str, sealed: Option<crate::run_store::SealedRun>) {
        let Some(sealed) = sealed else {
            return;
        };
        self.emit(RuntimeEvent::run_recorded(
            job_id,
            sealed.run_id.clone(),
            sealed.root_hash.clone(),
            sealed.event_count,
        ));
        // L1: record that the run occurred (INV-R1 — attests occurrence, asserts nothing
        // about correctness). No diff hash is available at the delegate seal tail, so
        // `DiffRecorded` is honestly skipped rather than fabricated.
        let kind = nerve_core::ledger::LedgerKind::RunRecorded {
            run_id: sealed.run_id,
            run_root_hash: sealed.root_hash,
            agent: sealed.agent,
            task_hash: sealed.task_hash,
            event_count: sealed.event_count,
        };
        if let Some(record) =
            crate::ledger_store::append_evidence(self.ledger_store().as_ref(), kind)
        {
            self.emit_ledger_appended(&record);
        }
    }

    /// Wave 3: lift a turn's verbatim raw stream lines into LIVE structured
    /// [`DelegateAgent`](RuntimeEvent::DelegateAgent) per-tool rows and emit them
    /// keyed by `job_id` — the wire analogue of the persisted L0 tool index, so a
    /// GUI/TUI renders per-tool rows instead of only the opaque `DelegateProgress`
    /// text tail. Reuses the pure `tool_events.rs` lift (unknown agent → no rows);
    /// `DelegateProgress` is unaffected (additive).
    pub(super) fn emit_delegate_tool_rows(
        &self,
        job_id: &str,
        resolved: DelegateAgent,
        raw_lines: &[String],
        turn: u64,
    ) {
        for line in raw_lines {
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                emit_tool_event_rows(resolved, &value, turn, &self.emit, job_id);
            }
        }
    }
}

/// Emit the LIVE structured `DelegateAgent` per-tool rows (Wave 3) for one parsed
/// stream line: lift its tool calls (unknown agent → none) and emit each as
/// a `delegate_agent` event keyed by `job_id`, alongside the retained
/// `DelegateProgress` text tail. Flat (free function) so the streaming closures stay
/// within the nesting budget.
fn emit_tool_event_rows(
    resolved: DelegateAgent,
    value: &Value,
    turn: u64,
    emit: &Arc<EventEmitter>,
    job_id: &str,
) {
    for kind in delegate_runtime::parse_tool_events(resolved, value, turn) {
        if let Some(ae) = delegate_runtime::tool_event_to_agent_event(&kind) {
            emit(RuntimeEvent::delegate_agent(job_id.to_string(), ae));
        }
    }
}

/// Build a [`UsageUpdated`](nerve_core::provenance::EventKind::UsageUpdated) event
/// for one turn from a parsed [`DelegateUsage`](delegate_runtime::DelegateUsage) +
/// reported USD cost. Used by the live capture path; cost is stored as integer
/// micro-USD (no floats in the hashed bytes — INV-R2).
fn delegate_usage_event(
    turn: u64,
    usage: &delegate_runtime::DelegateUsage,
    cost_usd: Option<f64>,
) -> nerve_core::provenance::EventKind {
    nerve_core::provenance::EventKind::UsageUpdated {
        turn,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_creation_tokens: usage.cache_creation_tokens,
        cost_micro_usd: crate::run_store::cost_to_micro_usd(cost_usd),
    }
}
