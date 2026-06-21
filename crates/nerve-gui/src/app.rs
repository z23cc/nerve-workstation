//! Chat surface + approvals + model picker + a sidebar conversation list.
//!
//! In-memory multi-chat: the sidebar lists the conversations started this
//! browser session; switching swaps the active transcript + session. Each chat
//! drives the `session.*` family over `/rpc` + `/events`, streaming assistant
//! turns from the SSE stream (deserialized into the EXACT `nerve_proto` types —
//! the single protocol authority), with an approval modal (`session.respond`).
//! Styling is a Codex-inspired native desktop surface, no proprietary assets.

use crate::render::render_turn;
use crate::rpc::{daemon_token, open_events, start_job, start_job_await};
use leptos::prelude::*;
use nerve_proto::{AgentEventKind, RuntimeEvent};
use serde_json::json;

const DEFAULT_PROVIDER: &str = "claude";
const DEFAULT_MODEL: &str = "claude-opus-4-8";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    User,
    Assistant,
}

#[derive(Clone)]
pub(crate) struct ToolCard {
    pub(crate) tool: String,
    pub(crate) ok: Option<bool>,
    pub(crate) output: String,
}

#[derive(Clone)]
pub(crate) struct Turn {
    pub(crate) role: Role,
    pub(crate) text: String,
    pub(crate) reasoning: String,
    pub(crate) tools: Vec<ToolCard>,
    pub(crate) streaming: bool,
}

impl Turn {
    fn user(text: String) -> Self {
        Self {
            role: Role::User,
            text,
            reasoning: String::new(),
            tools: Vec::new(),
            streaming: false,
        }
    }
    fn assistant_streaming() -> Self {
        Self {
            role: Role::Assistant,
            text: String::new(),
            reasoning: String::new(),
            tools: Vec::new(),
            streaming: true,
        }
    }
}

/// One conversation in the sidebar list (in-memory for this browser session).
#[derive(Clone)]
struct Chat {
    title: String,
    session: Option<String>,
    turns: Vec<Turn>,
    streaming: bool,
}

impl Chat {
    fn new() -> Self {
        Self {
            title: "New thread".into(),
            session: None,
            turns: Vec::new(),
            streaming: false,
        }
    }
}

/// A pending tool-permission decision (carries its own session_id so the reply
/// targets the right chat even if the user has switched conversations).
#[derive(Clone)]
struct ApprovalReq {
    session_id: String,
    request_id: String,
    tool: String,
    preview: String,
    tier: String,
}

#[component]
pub fn App() -> impl IntoView {
    let token = StoredValue::new(daemon_token());
    let chats = RwSignal::new(vec![Chat::new()]);
    let active = RwSignal::new(0usize);
    let input = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let approval = RwSignal::new(None::<ApprovalReq>);
    let provider = RwSignal::new(DEFAULT_PROVIDER.to_string());
    let model = RwSignal::new(DEFAULT_MODEL.to_string());
    let inspector_open = RwSignal::new(false);

    Effect::new(move |_| {
        if let Some(tok) = token.get_value() {
            let _ = open_events(&tok, move |event| route_event(event, chats, approval));
        } else {
            error.set(Some(
                "no daemon token — open the daemon's URL (or append #token=…)".into(),
            ));
        }
    });

    // Whether the active chat is mid-turn (drives Send⇄Stop).
    let active_busy = move || chats.with(|cs| cs.get(active.get()).is_some_and(|c| c.streaming));

    let send = move || {
        let Some(tok) = token.get_value() else { return };
        let text = input.get_untracked().trim().to_string();
        if text.is_empty() || active_busy() {
            return;
        }
        input.set(String::new());
        error.set(None);
        let idx = active.get_untracked();
        chats.update(|cs| {
            if let Some(c) = cs.get_mut(idx) {
                if c.turns.is_empty() {
                    c.title = truncate_title(&text);
                }
                c.turns.push(Turn::user(text.clone()));
                c.turns.push(Turn::assistant_streaming());
                c.streaming = true;
            }
        });
        let (prov, mdl) = (provider.get_untracked(), model.get_untracked());
        leptos::task::spawn_local(async move {
            let session_id = match ensure_session(&tok, chats, idx, &prov, &mdl).await {
                Ok(id) => id,
                Err(err) => return fail_chat(chats, idx, error, err),
            };
            let cmd = json!({"kind": "session.message", "session_id": session_id, "text": text});
            if let Err(err) = start_job(&tok, cmd).await {
                fail_chat(chats, idx, error, format!("session.message: {err}"));
            }
        });
    };

    let stop = move || {
        let Some(tok) = token.get_value() else { return };
        let idx = active.get_untracked();
        let Some(session_id) =
            chats.with_untracked(|cs| cs.get(idx).and_then(|c| c.session.clone()))
        else {
            return;
        };
        leptos::task::spawn_local(async move {
            let _ = start_job(
                &tok,
                json!({"kind": "session.interrupt", "session_id": session_id}),
            )
            .await;
        });
    };

    let apply_model = move || {
        let Some(tok) = token.get_value() else { return };
        let idx = active.get_untracked();
        let Some(session_id) =
            chats.with_untracked(|cs| cs.get(idx).and_then(|c| c.session.clone()))
        else {
            return;
        };
        let (prov, mdl) = (provider.get_untracked(), model.get_untracked());
        leptos::task::spawn_local(async move {
            let cmd = json!({"kind": "session.set_model", "session_id": session_id, "provider": prov, "model": mdl});
            let _ = start_job(&tok, cmd).await;
        });
    };

    let new_chat = move |_| {
        let mut idx = 0;
        chats.update(|cs| {
            cs.push(Chat::new());
            idx = cs.len() - 1;
        });
        active.set(idx);
        input.set(String::new());
        error.set(None);
    };

    let toggle_inspector = move |_| inspector_open.update(|open| *open = !*open);

    // Only flips when the active chat goes empty↔non-empty, so the composer is not
    // re-created (and the textarea does not lose focus) on every streaming delta.
    let empty = Memo::new(move |_| {
        chats.with(|cs| {
            cs.get(active.get())
                .map(|c| c.turns.is_empty())
                .unwrap_or(true)
        })
    });

    // The Codex-style composer: a large rounded box (textarea + an inline tool row)
    // with context pills beneath. Reused as the centered hero (empty state) and as
    // the docked bar (active conversation). Copy closure → usable in both branches.
    let composer = move || {
        view! {
            <div class="composer-stack">
                <div class="composer-box">
                    <textarea
                        id="message"
                        name="message"
                        class="input"
                        rows="1"
                        prop:value=move || input.get()
                        on:input=move |ev| input.set(event_target_value(&ev))
                        on:keydown=move |ev| {
                            if ev.key() == "Enter" && !ev.shift_key() {
                                ev.prevent_default();
                                send();
                            }
                        }
                        placeholder="Ask Nerve to build…"
                    ></textarea>
                    <div class="composer-tools">
                        <button class="tool-btn" title="Attach context">"+"</button>
                        <span class="access-pill" title="Approval mode">"Full access"</span>
                        <span class="tool-spacer"></span>
                        <span class="effort">{move || format!("{} · high", model.get())}</span>
                        {move || if active_busy() {
                            view! { <button class="send stop" title="Stop" on:click=move |_| stop()>"■"</button> }.into_any()
                        } else {
                            view! { <button class="send" title="Send" on:click=move |_| send()>"↑"</button> }.into_any()
                        }}
                    </div>
                </div>
                <div class="context-pills">
                    <span class="ctx-pill">"⌂ nerve-workstation"</span>
                    <span class="ctx-pill">"Local"</span>
                    <span class="ctx-pill">"⎇ main"</span>
                </div>
            </div>
        }
    };

    view! {
        <div id="nerve-shell" class:with-inspector=move || inspector_open.get()>
            <aside class="sidebar">
                <div class="brand"><span class="spark">"N"</span><span>"Nerve"</span></div>
                <button class="newchat" title="New thread" on:click=new_chat>
                    <span class="plus">"+"</span>"New thread"
                </button>
                <div class="nav">
                    <div class="nav-row on"><span class="nav-icon">"☰"</span><span>"Threads"</span></div>
                    <div class="nav-row"><span class="nav-icon">"◌"</span><span>"Chats"</span></div>
                    <div class="nav-row"><span class="nav-icon">"⌁"</span><span>"Automations"</span></div>
                    <div class="nav-row"><span class="nav-icon">"✦"</span><span>"Skills"</span></div>
                </div>
                <div class="rail-label">"Projects"</div>
                <div class="project-row"><span class="project-dot"></span><span>"nerve-workstation"</span></div>
                <div class="rail-label">"Threads"</div>
                <div class="rail">
                    {move || {
                        let cur = active.get();
                        chats.get().into_iter().enumerate().map(|(i, c)| {
                            let cls = if i == cur { "rail-row on" } else { "rail-row" };
                            let live = c.session.is_some();
                            let title = c.title;
                            view! {
                                <button class=cls on:click=move |_| active.set(i)>
                                    <span class="rail-dot" class:live=live></span>
                                    <span class="rail-title">{title}</span>
                                </button>
                            }
                        }).collect_view()
                    }}
                </div>
                <div class="spacer"></div>
                <div class="status-row">
                    {move || if active_busy() {
                        view! { <span class="dot busy"></span>"running" }.into_any()
                    } else {
                        view! { <span class="dot idle"></span>"runtime v4" }.into_any()
                    }}
                </div>
            </aside>
            <main class="main chat">
                <div class="topbar">
                    <div class="topbar-title">
                        {move || chats.with(|cs| cs.get(active.get()).map(|c| c.title.clone()).unwrap_or_default())}
                    </div>
                    <div class="picker">
                        <details class="model-menu">
                            <summary class="model-pill">
                                <span>{move || format!("{} · {}", provider.get(), model.get())}</span>
                            </summary>
                            <div class="model-popover">
                                <label>"Provider"<input id="provider" name="provider" class="pick-in" prop:value=move || provider.get()
                                    on:input=move |ev| provider.set(event_target_value(&ev)) title="provider" /></label>
                                <label>"Model"<input id="model" name="model" class="pick-in wide" prop:value=move || model.get()
                                    on:input=move |ev| model.set(event_target_value(&ev)) title="model" /></label>
                                <button class="pick-apply" on:click=move |_| apply_model()>"Apply"</button>
                            </div>
                        </details>
                        <button class="icon-btn" title="Task pane" on:click=toggle_inspector>"⊞"</button>
                    </div>
                </div>
                {move || if empty.get() {
                    view! {
                        <div class="hero">
                            <h1 class="hero-title">"What should we build?"</h1>
                            <div class="hero-composer">{composer()}</div>
                        </div>
                    }.into_any()
                } else {
                    view! {
                        <div class="transcript">
                            {move || chats.with(|cs| cs.get(active.get()).map(|c| c.turns.clone()).unwrap_or_default())
                                .into_iter().map(render_turn).collect_view()}
                            {move || error.get().map(|e| view! { <div class="turn-error">{e}</div> })}
                        </div>
                        <div class="composer-dock">{composer()}</div>
                    }.into_any()
                }}
            </main>
            {move || inspector_open.get().then(|| view! {
                <aside class="inspector">
                    <div class="inspector-head">
                        <span class="inspector-title">"Plan"</span>
                        <span class="inspector-chip">"Local"</span>
                    </div>
                    <div class="inspector-tabs">
                        <button class="inspector-tab on">"Plan"</button>
                        <button class="inspector-tab">"Files"</button>
                        <button class="inspector-tab">"Changes"</button>
                    </div>
                    <div class="inspector-body">
                        <div class="plan-step done"><span></span><p>"Read workspace context"</p></div>
                        <div class="plan-step on"><span></span><p>"Work in the active thread"</p></div>
                        <div class="plan-step"><span></span><p>"Review generated changes"</p></div>
                    </div>
                </aside>
            })}
            {move || approval.get().map(|req| view! {
                <ApprovalModal req=req token=token approval=approval />
            })}
        </div>
    }
}

#[component]
fn ApprovalModal(
    req: ApprovalReq,
    token: StoredValue<Option<String>>,
    approval: RwSignal<Option<ApprovalReq>>,
) -> impl IntoView {
    let decide = move |decision: &'static str| respond(token, approval, decision);
    view! {
        <div class="modal-scrim">
            <div class="modal">
                <div class="modal-head">
                    <span class="modal-title">"Allow "<b>{req.tool.clone()}</b></span>
                    <span class=format!("tier {}", req.tier.to_lowercase())>{req.tier.clone()}</span>
                </div>
                {(!req.preview.is_empty()).then(|| view! { <pre class="modal-preview">{req.preview.clone()}</pre> })}
                <div class="modal-actions">
                    <button class="btn allow" on:click=move |_| decide("allow")>"Allow"</button>
                    <button class="btn" on:click=move |_| decide("allow_always")>"Always"</button>
                    <button class="btn" on:click=move |_| decide("deny")>"Deny"</button>
                    <button class="btn danger" on:click=move |_| decide("deny_always")>"Deny always"</button>
                </div>
            </div>
        </div>
    }
}

/// Send a `session.respond` decision (to the approval's own session) and clear the modal.
fn respond(
    token: StoredValue<Option<String>>,
    approval: RwSignal<Option<ApprovalReq>>,
    decision: &'static str,
) {
    let req = approval.get_untracked();
    approval.set(None);
    let (Some(tok), Some(req)) = (token.get_value(), req) else {
        return;
    };
    leptos::task::spawn_local(async move {
        let cmd = json!({
            "kind": "session.respond",
            "session_id": req.session_id,
            "request_id": req.request_id,
            "decision": decision,
        });
        let _ = start_job(&tok, cmd).await;
    });
}

/// Return the chat's live session id, starting one (`session.start`) if needed.
async fn ensure_session(
    token: &str,
    chats: RwSignal<Vec<Chat>>,
    idx: usize,
    provider: &str,
    model: &str,
) -> Result<String, String> {
    if let Some(id) = chats.with_untracked(|cs| cs.get(idx).and_then(|c| c.session.clone())) {
        return Ok(id);
    }
    let cmd = json!({"kind": "session.start", "provider": provider, "model": model});
    let result = start_job_await(token, cmd)
        .await
        .map_err(|err| format!("session.start: {err}"))?;
    let id = result
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| "session.start returned no session_id".to_string())?;
    chats.update(|cs| {
        if let Some(c) = cs.get_mut(idx) {
            c.session = Some(id.clone());
        }
    });
    Ok(id)
}

/// Mark the chat's in-flight turn failed and surface the error.
fn fail_chat(
    chats: RwSignal<Vec<Chat>>,
    idx: usize,
    error: RwSignal<Option<String>>,
    message: String,
) {
    chats.update(|cs| {
        if let Some(c) = cs.get_mut(idx) {
            c.streaming = false;
            if let Some(turn) = c.turns.last_mut() {
                turn.streaming = false;
            }
        }
    });
    error.set(Some(message));
}

/// Route one `RuntimeEvent` from the SSE stream into the chat owning its session.
fn route_event(
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

/// A short sidebar title from the first user message.
fn truncate_title(text: &str) -> String {
    let line = text.lines().next().unwrap_or(text).trim();
    let mut title: String = line.chars().take(40).collect();
    if line.chars().count() > 40 {
        title.push('…');
    }
    if title.is_empty() {
        "New thread".into()
    } else {
        title
    }
}
