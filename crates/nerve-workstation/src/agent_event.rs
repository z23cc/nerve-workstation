//! Shared mapping from agent-loop events to the protocol's transport-neutral
//! [`AgentEventKind`]. Used by both the job adapter (`agent.run`) and the
//! session manager (`session.*`), which differ only in the outer event wrapper
//! (`RuntimeEvent::agent` vs `RuntimeEvent::session_agent`).

use nerve_agent::AgentEvent;
use nerve_runtime::AgentEventKind;

/// Translate an [`AgentEvent`] into a protocol [`AgentEventKind`], or `None` for
/// events that carry no client-visible step (the terminal `Done`).
pub(crate) fn agent_event_kind(event: AgentEvent) -> Option<AgentEventKind> {
    Some(match event {
        AgentEvent::TurnStarted(turn) => AgentEventKind::TurnStarted {
            turn: u64::from(turn),
        },
        AgentEvent::AssistantText(text) => AgentEventKind::Message { text },
        AgentEvent::Reasoning(text) => AgentEventKind::Reasoning { text },
        AgentEvent::ToolStarted { name, args } => AgentEventKind::ToolStarted {
            tool: name,
            arguments: args,
        },
        AgentEvent::ToolFinished { name, ok, output } => AgentEventKind::ToolFinished {
            tool: name,
            ok,
            output,
        },
        AgentEvent::Interrupted(reason) => AgentEventKind::Interrupted { reason },
        AgentEvent::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
        } => AgentEventKind::Usage {
            input_tokens: u64::from(input_tokens),
            output_tokens: u64::from(output_tokens),
            // Report cache counts only when the provider actually saw caching, so
            // a zero stays an explicit "no cache activity" omission on the wire.
            cache_read_tokens: nonzero(cache_read_tokens),
            cache_creation_tokens: nonzero(cache_creation_tokens),
        },
        // `ToolCallDelta` is a job/session-scoped `RuntimeEvent`, not an
        // `AgentEventKind`; the caller maps it directly (see the host job/session
        // adapters). It has no structured agent-loop step here.
        AgentEvent::ToolCallDelta { .. } => return None,
        AgentEvent::Done { .. } => return None,
    })
}

/// Map a `0` cache count to `None` (omit) and any positive count to `Some`, so
/// the optional protocol fields distinguish "no caching" from "not reported".
fn nonzero(tokens: u32) -> Option<u64> {
    (tokens > 0).then(|| u64::from(tokens))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_threads_nonzero_cache_tokens() {
        let kind = agent_event_kind(AgentEvent::Usage {
            input_tokens: 100,
            output_tokens: 20,
            cache_read_tokens: 1_024,
            cache_creation_tokens: 0,
        })
        .expect("usage maps to a kind");
        match kind {
            AgentEventKind::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
            } => {
                assert_eq!(input_tokens, 100);
                assert_eq!(output_tokens, 20);
                // Non-zero read is reported; zero creation is omitted.
                assert_eq!(cache_read_tokens, Some(1_024));
                assert_eq!(cache_creation_tokens, None);
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_delta_has_no_agent_kind() {
        // It is mapped to a job-scoped RuntimeEvent by the caller, not here.
        assert!(
            agent_event_kind(AgentEvent::ToolCallDelta {
                name: "search".into(),
                arguments: serde_json::json!({"q": "x"}),
            })
            .is_none()
        );
    }
}
