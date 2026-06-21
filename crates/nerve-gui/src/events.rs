//! SSE event routing: fold each `RuntimeEvent` from `/events` into the chat
//! that owns its session. Split out of `app.rs` to stay under the file-size
//! gate; the chat state types live in [`crate::app`].

use crate::app::{ApprovalReq, Chat, Role, ToolCard, Turn};
use leptos::prelude::*;
use nerve_proto::{AgentEventKind, RuntimeEvent};

/// Route one `RuntimeEvent` from the SSE stream into the chat owning its session.
pub(crate) fn route_event(
    event: RuntimeEvent,
    chats: RwSignal<Vec<Chat>>,
    approval: RwSignal<Option<ApprovalReq>>,
) {
    match event {
        RuntimeEvent::SessionIdle { session_id } => with_session(chats, &session_id, |c| {
            c.streaming = false;
            if let Some(turn) = c.turns.last_mut() {
                turn.streaming = false;
            }
        }),
        RuntimeEvent::SessionClosed { session_id } => {
            with_session(chats, &session_id, |c| c.streaming = false)
        }
        RuntimeEvent::SessionAgent { session_id, event } => {
            with_session(chats, &session_id, |c| apply_agent_event(event, c));
        }
        RuntimeEvent::ApprovalRequested {
            session_id,
            request_id,
            tool,
            preview,
            tier,
            ..
        } => {
            // Only surface approvals for a session we own.
            if chats.with_untracked(|cs| {
                cs.iter()
                    .any(|c| c.session.as_deref() == Some(session_id.as_str()))
            }) {
                approval.set(Some(ApprovalReq {
                    session_id,
                    request_id,
                    tool,
                    preview,
                    tier: format!("{tier:?}"),
                }));
            }
        }
        _ => {}
    }
}

/// Apply `f` to the chat whose session matches `session_id`, if any.
fn with_session(chats: RwSignal<Vec<Chat>>, session_id: &str, f: impl FnOnce(&mut Chat)) {
    chats.update(|cs| {
        if let Some(c) = cs
            .iter_mut()
            .find(|c| c.session.as_deref() == Some(session_id))
        {
            f(c);
        }
    });
}

/// Fold a single `AgentEventKind` into the chat's current (streaming) assistant turn.
fn apply_agent_event(event: AgentEventKind, chat: &mut Chat) {
    let needs_turn =
        !matches!(chat.turns.last(), Some(t) if t.role == Role::Assistant && t.streaming);
    if needs_turn {
        chat.turns.push(Turn::assistant_streaming());
    }
    let Some(turn) = chat.turns.last_mut() else {
        return;
    };
    match event {
        AgentEventKind::Message { text } => turn.text.push_str(&text),
        AgentEventKind::Reasoning { text } => turn.reasoning.push_str(&text),
        AgentEventKind::ToolStarted { tool, .. } => turn.tools.push(ToolCard {
            tool,
            ok: None,
            output: String::new(),
        }),
        AgentEventKind::ToolFinished { tool, ok, output } => {
            match turn
                .tools
                .iter_mut()
                .rev()
                .find(|card| card.tool == tool && card.ok.is_none())
            {
                Some(card) => {
                    card.ok = Some(ok);
                    card.output = output;
                }
                None => turn.tools.push(ToolCard {
                    tool,
                    ok: Some(ok),
                    output,
                }),
            }
        }
        AgentEventKind::Interrupted { .. } => {
            turn.streaming = false;
            chat.streaming = false;
        }
        AgentEventKind::TurnStarted { .. } | AgentEventKind::Usage { .. } => {}
    }
}
