//! DA-7: delegate **role** presets — the host-side behavior a [`DelegateRole`]
//! maps to, layered on top of the raw agent CLI recipe.
//!
//! The protocol carries only the *vocabulary* ([`DelegateRole`] in `nerve-proto`);
//! the prompt text and posture it expands to are host policy and live here. This
//! keeps the protocol minimal and lets the preset evolve without a wire change.
//!
//! ## The `scout` role (the FastContext pattern, on an existing CLI)
//!
//! [`DelegateRole::Scout`] turns a delegated agent into a **read-only repository
//! explorer**: it wraps the caller's query in an explore-and-cite instruction and
//! **forces read-only autonomy**, so the agent returns compact `path:line-range`
//! citations instead of editing. The point is token economy — the main agent
//! delegates "where does X live?" to a cheap sub-agent and gets focused evidence
//! back rather than burning its own context window on broad reads. The discipline
//! (broad→narrow, parallel reads, citations, no edits) is carried entirely by the
//! prompt, so any existing CLI agent runs it without a special model.
//!
//! The transform is applied once at the single delegate fan-in
//! ([`crate::jobs`] `run_delegate_start`), so both the one-shot and live-session
//! paths inherit it.

use nerve_runtime::{DelegateAutonomy, DelegateRole};

/// The read-only exploration instruction prepended to a `scout` delegate's task.
/// Adapted from the FastContext exploration discipline: broad-then-narrow search,
/// parallel read-only tool calls, and a precise `path:line-range` citation answer.
pub(crate) const SCOUT_SYSTEM_PROMPT: &str = "\
You are a READ-ONLY repository exploration sub-agent. Your ONLY job is to LOCATE \
the code relevant to the query below and report precise citations. You do NOT \
edit files, run build/test/format commands, or attempt to solve the task — \
another agent does that with the evidence you return.

How to explore:
- Start broad, then narrow down. If the first search misses, try another strategy \
(different identifiers, file globs, call sites).
- Prefer issuing multiple read/search calls in PARALLEL in one turn; speed matters.
- Search broadly when you don't know where something lives; read a file directly \
when you already know its path.
- Stop as soon as the evidence is dense enough to answer confidently — do not \
over-explore.

How to answer — end your final message with a block exactly like:
<final_answer>
/abs/path/to/file.ext:START-END — one short note on why this is relevant
/abs/path/to/other.ext:LINE — ...
</final_answer>
Use absolute paths and 1-based line ranges. Keep prose minimal; the citations are \
the deliverable. Do not modify anything.

Query:
";

/// Expand a [`DelegateRole`] into the effective `(task, autonomy)` for a delegated
/// run. [`DelegateRole::Standard`] is a passthrough; [`DelegateRole::Scout`] wraps
/// the task in [`SCOUT_SYSTEM_PROMPT`] and **forces** read-only autonomy regardless
/// of what the caller requested (a scout is read-only by definition).
#[must_use]
pub(crate) fn apply_role(
    role: DelegateRole,
    task: &str,
    autonomy: DelegateAutonomy,
) -> (String, DelegateAutonomy) {
    match role {
        DelegateRole::Standard => (task.to_string(), autonomy),
        DelegateRole::Scout => (
            format!("{SCOUT_SYSTEM_PROMPT}{task}"),
            DelegateAutonomy::ReadOnly,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_role_is_passthrough() {
        let (task, autonomy) = apply_role(
            DelegateRole::Standard,
            "do the thing",
            DelegateAutonomy::Edit,
        );
        assert_eq!(task, "do the thing");
        // Standard never overrides the caller's autonomy.
        assert_eq!(autonomy, DelegateAutonomy::Edit);
    }

    #[test]
    fn scout_role_wraps_task_and_forces_read_only() {
        // Even when the caller asks for Full, a scout is forced read-only.
        let (task, autonomy) = apply_role(
            DelegateRole::Scout,
            "where is auth handled?",
            DelegateAutonomy::Full,
        );
        assert_eq!(autonomy, DelegateAutonomy::ReadOnly);
        assert!(
            task.starts_with(SCOUT_SYSTEM_PROMPT),
            "scout task must be prefixed with the exploration instruction"
        );
        assert!(
            task.ends_with("where is auth handled?"),
            "the caller's query is appended verbatim after the instruction"
        );
        // The citation contract is conveyed to the agent.
        assert!(task.contains("<final_answer>"));
        assert!(task.contains("READ-ONLY"));
    }

    #[test]
    fn scout_forces_read_only_even_from_read_only() {
        let (_, autonomy) = apply_role(DelegateRole::Scout, "q", DelegateAutonomy::ReadOnly);
        assert_eq!(autonomy, DelegateAutonomy::ReadOnly);
    }
}
