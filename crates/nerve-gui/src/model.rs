//! Chat-state types shared across the GUI.
//!
//! Extracted from `app.rs` so the root `App` view fn stays under the file-size
//! gate, and so the per-turn reactive cell ([`TurnHandle`]) has a natural home.
//!
//! The streaming transcript is driven by **per-turn signals**: each [`Turn`] lives
//! behind its own [`ArcRwSignal`], paired with a stable `id`. An SSE delta updates
//! exactly one turn's signal (repainting one row), and the keyed transcript `<For>`
//! diffs by `id` so finished rows never re-render or re-parse markdown. `TurnHandle`
//! serializes **transparently** as a bare `Turn`, so persisted conversation history
//! stays a plain `Vec<Turn>` on disk and host tests can still build
//! `Chat { turns: Vec::new(), .. }`.

use crate::chat_backend::{default_agent, default_chat_backend};
use leptos::prelude::*;
use std::cell::Cell;

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Role {
    User,
    Assistant,
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ToolCard {
    pub(crate) tool: String,
    pub(crate) ok: Option<bool>,
    #[serde(default)]
    pub(crate) input: String,
    pub(crate) output: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Turn {
    pub(crate) role: Role,
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) reasoning: String,
    #[serde(default)]
    pub(crate) tools: Vec<ToolCard>,
    // Runtime-only: a restored turn is never mid-stream.
    #[serde(skip)]
    pub(crate) streaming: bool,
}

impl Turn {
    pub(crate) fn user(text: String) -> Self {
        Self {
            role: Role::User,
            text,
            reasoning: String::new(),
            tools: Vec::new(),
            streaming: false,
        }
    }
    pub(crate) fn assistant_streaming() -> Self {
        Self {
            role: Role::Assistant,
            text: String::new(),
            reasoning: String::new(),
            tools: Vec::new(),
            streaming: true,
        }
    }
}

// Process-wide monotonic turn id. `ArcRwSignal` is owner-independent (and `!Send`
// on wasm), so a `thread_local` cell is the lock-free, runtime-free counter — and
// it keeps an id stable across a streaming turn becoming finished (only the `Turn`
// inside the signal changes; the id never does, so the `<For>` row is not re-keyed).
thread_local! {
    static TURN_SEQ: Cell<u64> = const { Cell::new(0) };
}

fn next_turn_id() -> u64 {
    TURN_SEQ.with(|c| {
        let n = c.get().wrapping_add(1);
        c.set(n);
        n
    })
}

/// A stable per-turn identity + the reactive cell the transcript row reads.
///
/// Serializes transparently as a bare [`Turn`] (the `id`/`sig` are runtime-only),
/// so on-disk history is unchanged and `load_chats` round-trips through
/// [`TurnHandle::new`], minting a fresh id + signal per restored turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TurnHandle {
    pub(crate) id: u64,
    pub(crate) sig: ArcRwSignal<Turn>,
}

impl TurnHandle {
    pub(crate) fn new(turn: Turn) -> Self {
        Self {
            id: next_turn_id(),
            sig: ArcRwSignal::new(turn),
        }
    }

    /// A snapshot of the inner turn (cold-path reads: persistence, export,
    /// inspector, host tests). `get_untracked` works off-DOM, so this is host-safe.
    pub(crate) fn get(&self) -> Turn {
        self.sig.get_untracked()
    }
}

impl serde::Serialize for TurnHandle {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.sig.get_untracked().serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for TurnHandle {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::new(Turn::deserialize(deserializer)?))
    }
}

/// One conversation in the sidebar list (in-memory for this browser session).
/// `session` is the backend session id (`session.start` id or the delegate
/// live-session job id); `turn_job` is the in-flight turn job id, used to stop it.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Chat {
    pub(crate) title: String,
    #[serde(default = "default_chat_backend")]
    pub(crate) backend: String,
    /// The external CLI agent bound to this thread (claude / codex / gemini),
    /// captured at first send. Drives the sidebar agent badge.
    #[serde(default = "default_agent")]
    pub(crate) agent: String,
    // Runtime-only (server-side session + in-flight job + streaming flag): never
    // persisted. A restored chat is offline history until the next message, which
    // opens a fresh backend session under the same thread.
    #[serde(skip)]
    pub(crate) session: Option<String>,
    #[serde(skip)]
    pub(crate) turn_job: Option<String>,
    #[serde(default)]
    pub(crate) turns: Vec<TurnHandle>,
    #[serde(skip)]
    pub(crate) streaming: bool,
    /// Epoch-ms of the last activity (created / last message). Drives the rail's
    /// relative timestamp and recency sort.
    #[serde(default)]
    pub(crate) updated_ms: f64,
}

impl Chat {
    pub(crate) fn new_with_backend(backend: impl Into<String>) -> Self {
        Self {
            title: "New thread".into(),
            backend: backend.into(),
            agent: default_agent(),
            session: None,
            turn_job: None,
            turns: Vec::new(),
            streaming: false,
            updated_ms: js_sys::Date::now(),
        }
    }
}

/// A pending tool-permission decision (carries its own session_id so the reply
/// targets the right chat even if the user has switched conversations).
#[derive(Clone)]
pub(crate) struct ApprovalReq {
    pub(crate) session_id: String,
    pub(crate) request_id: String,
    pub(crate) tool: String,
    pub(crate) preview: String,
    pub(crate) tier: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_handle_serializes_as_bare_turn() {
        let mut turn = Turn::user("hi".into());
        turn.reasoning = "r".into();
        let handle = TurnHandle::new(turn);
        let json = serde_json::to_value(&handle).expect("serialize");
        // No `id`/`sig` keys leak; it is exactly the Turn shape.
        assert!(json.get("id").is_none());
        assert_eq!(json.get("text").and_then(|t| t.as_str()), Some("hi"));
        assert_eq!(json.get("role").and_then(|r| r.as_str()), Some("User"));
    }

    #[test]
    fn turn_handle_round_trips_through_a_vec() {
        let chat_json = serde_json::json!({
            "title": "t",
            "turns": [
                { "role": "Assistant", "text": "answer", "reasoning": "why", "tools": [] }
            ]
        });
        let chat: Chat = serde_json::from_value(chat_json).expect("deserialize");
        assert_eq!(chat.turns.len(), 1);
        let turn = chat.turns[0].get();
        assert_eq!(turn.text, "answer");
        assert_eq!(turn.reasoning, "why");
        assert!(!turn.streaming, "restored turns are never mid-stream");
        // Each restored handle has its own signal + a fresh id.
        assert!(chat.turns[0].id > 0);
    }

    #[test]
    fn distinct_handles_have_distinct_ids() {
        let a = TurnHandle::new(Turn::user("a".into()));
        let b = TurnHandle::new(Turn::user("b".into()));
        assert_ne!(a.id, b.id);
        assert_ne!(a, b, "PartialEq is by id + signal identity");
    }
}
