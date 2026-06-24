//! Keyed, per-turn-reactive transcript.
//!
//! Each row reads its own `ArcRwSignal<Turn>` (via a scoped `RwSignal`), so an
//! SSE delta repaints exactly one row. The `<For>` is keyed by the stable turn
//! `id`, so finished rows are never re-created — their markdown is parsed once,
//! on completion, and never again while the conversation grows. A `Memo` over the
//! handle list absorbs the per-delta `chats` notifications (the id-list is
//! unchanged by a text delta, so the keyed list does not re-diff).
//!
//! Streaming turns render their text as an **escaped text node** (never
//! `inner_html`), so streaming can never become an XSS vector; only the finalized
//! turn routes through `markdown_to_html`, which already sanitizes URLs.

use crate::model::{Chat, Role, Turn, TurnHandle};
use crate::render::{copy_status, markdown_to_html, render_tool, turn_copy_button};
use crate::scroll::ScrollAnchor;
use leptos::prelude::*;

#[component]
pub(crate) fn Transcript(
    chats: RwSignal<Vec<Chat>>,
    active: RwSignal<usize>,
    anchor: ScrollAnchor,
) -> impl IntoView {
    // The keyed handle list for the active chat. Cloning `Vec<TurnHandle>` is an
    // id copy + Arc refcount bump per turn (NO `Turn` deep clone, NO markdown), and
    // `TurnHandle`'s `PartialEq` is by id + signal identity — so a text delta
    // (which leaves the id-list unchanged) lets this `Memo` short-circuit and the
    // `<For>` body never re-runs. Only append / clear / thread-switch move it.
    let handles = Memo::new(move |_| {
        chats.with(|cs| {
            cs.get(active.get())
                .map(|c| c.turns.clone())
                .unwrap_or_default()
        })
    });

    // Follow the bottom as content grows. `events.rs` folds every streaming delta
    // through `chats.update`, so tracking `chats`/`active` re-runs this on each
    // delta; the scroll write itself is rAF-coalesced inside the anchor. A thread
    // switch is explicit intent → snap and re-arm regardless of the pin.
    Effect::new(move |prev: Option<usize>| {
        let idx = active.get();
        // Subscribe to the active chat's content — events.rs folds every delta
        // through `chats.update`, so this re-runs on each one. A first mount or a
        // thread switch is explicit intent → re-pin; otherwise follow only when
        // already pinned (so a user reading history is never yanked down).
        let _ = chats.with(|cs| cs.get(idx).map(|c| c.turns.len()));
        if prev != Some(idx) {
            anchor.snap_to_bottom();
        } else {
            anchor.follow_if_pinned();
        }
        idx
    });

    view! {
        // Positioning context for the floating jump pill: its bottom edge is the
        // transcript's bottom (i.e. just above the composer dock) regardless of
        // composer height, so the pill never overlaps a multi-line composer.
        <div class="transcript-wrap">
            <div
                class="transcript"
                node_ref=anchor.container
                on:scroll=move |_| anchor.on_user_scroll()
                role="log"
                aria-label="Conversation transcript"
                aria-live="polite"
                aria-relevant="additions text"
                aria-atomic="false"
            >
                <For
                    each=move || handles.get()
                    key=|handle: &TurnHandle| handle.id
                    children=move |handle: TurnHandle| render_turn_reactive(handle)
                />
            </div>
            {move || {
                (!anchor.pinned.get()).then(|| view! {
                    <button
                        class="jump-latest"
                        type="button"
                        aria-label="Jump to latest message"
                        on:click=move |_| {
                            anchor.snap_to_bottom();
                            // The button removes itself on pin; hand focus to the
                            // composer rather than dropping it to <body>.
                            crate::dom::focus_message_input();
                        }
                    >
                        <span class="jump-latest-arrow" aria-hidden="true">"↓"</span>
                        "Jump to latest"
                    </button>
                })
            }}
        </div>
    }
}

/// Render one turn. The role is fixed for a turn's life, so it is read once;
/// user turns are static, assistant turns are reactive on their own signal.
fn render_turn_reactive(handle: TurnHandle) -> AnyView {
    let role = handle.sig.with_untracked(|t| t.role);
    match role {
        Role::User => render_user(&handle),
        Role::Assistant => render_assistant(RwSignal::from(handle.sig)),
    }
}

fn render_user(handle: &TurnHandle) -> AnyView {
    let text = handle.sig.with_untracked(|t| t.text.clone());
    let copy_note = RwSignal::new(String::new());
    view! {
        <div class="turn user" role="article" aria-label="User message">
            <div class="turn-actions user-actions">
                {turn_copy_button("Copy user message", text.clone(), copy_note)}
                {copy_status(copy_note)}
            </div>
            <div class="bubble" aria-label="User message text">{text}</div>
        </div>
    }
    .into_any()
}

fn render_assistant(sig: RwSignal<Turn>) -> AnyView {
    // Coarse-grained memos so the reasoning/tool fragments don't rebuild on every
    // text delta (which would collapse an opened <details>); only the text body
    // updates per delta. `streaming` flips exactly once (open → finalize).
    let streaming = Memo::new(move |_| sig.with(|t| t.streaming));
    let reasoning = Memo::new(move |_| sig.with(|t| t.reasoning.clone()));
    let tools = Memo::new(move |_| sig.with(|t| t.tools.clone()));
    let code_blocks = StoredValue::new(Vec::<String>::new());
    let copy_note = RwSignal::new(String::new());
    view! {
        <div
            class="turn assistant"
            role="article"
            aria-label=move || assistant_label(streaming.get())
            aria-busy=move || if streaming.get() { "true" } else { "false" }
        >
            <div class="turn-actions assistant-actions">
                {assistant_copy_button(sig, copy_note)}
                {copy_status(copy_note)}
            </div>
            {move || reasoning_view(reasoning.get())}
            {move || assistant_body(sig, streaming.get(), code_blocks, copy_note)}
            {move || tools.get().into_iter().map(render_tool).collect_view()}
            {move || streaming.get().then(|| view! { <span class="cursor" aria-hidden="true">"▋"</span> })}
        </div>
    }
    .into_any()
}

fn assistant_label(streaming: bool) -> &'static str {
    if streaming {
        "Assistant response, streaming"
    } else {
        "Assistant response"
    }
}

/// Copy button that reads the *latest* assistant text at click time (the text
/// grows while streaming), and disables itself until there is something to copy.
fn assistant_copy_button(sig: RwSignal<Turn>, note: RwSignal<String>) -> impl IntoView {
    view! {
        <button
            class="turn-copy"
            type="button"
            aria-label="Copy assistant response"
            disabled=move || sig.with(|t| t.text.is_empty())
            on:click=move |_| {
                let text = sig.with_untracked(|t| t.text.clone());
                crate::clipboard::copy_text_with_note(text, note, "Copied message.");
            }
        >"Copy"</button>
    }
}

fn reasoning_view(reasoning: String) -> Option<AnyView> {
    (!reasoning.is_empty()).then(|| {
        view! {
            <details class="reasoning" aria-label="Assistant reasoning details">
                <summary aria-label="Toggle assistant reasoning details">"Thought for this step"</summary>
                <pre aria-label="Assistant reasoning text">{reasoning}</pre>
            </details>
        }
        .into_any()
    })
}

/// The assistant body: while streaming, an escaped text node (no markdown parse,
/// no XSS surface) — or a "thinking" indicator before the first token; once
/// finalized, the single memoized `markdown_to_html` pass with copy-able code.
fn assistant_body(
    sig: RwSignal<Turn>,
    streaming: bool,
    code_blocks: StoredValue<Vec<String>>,
    copy_note: RwSignal<String>,
) -> AnyView {
    if streaming {
        let text = sig.with(|t| t.text.clone());
        if text.is_empty() {
            return view! {
                <div class="md md-streaming">
                    <span class="thinking" role="status" aria-label="Thinking">
                        <span class="thinking-dot"></span>
                        <span class="thinking-dot"></span>
                        <span class="thinking-dot"></span>
                    </span>
                </div>
            }
            .into_any();
        }
        return view! { <div class="md md-streaming">{text}</div> }.into_any();
    }
    let rendered = sig.with(|t| markdown_to_html(&t.text));
    code_blocks.set_value(rendered.code_blocks);
    let html = rendered.html;
    view! {
        <div
            class="md"
            inner_html=html
            on:click=move |ev| crate::render::copy_code_block(ev, code_blocks, copy_note)
        ></div>
    }
    .into_any()
}
