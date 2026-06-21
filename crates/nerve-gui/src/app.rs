//! Chat surface + approvals + agent picker + a sidebar conversation list.
//!
//! The chat backend is the local agent CLIs (claude = Claude Code, codex =
//! Codex, gemini) over the DELEGATE path — not the in-process subscription
//! providers. Each conversation drives `delegate.start` (first message, the job
//! id becomes the live session id), then `delegate.steer` (follow-ups), with
//! `delegate.close` to end it. Replies stream as `DelegateProgress` text chunks
//! and a turn ends on `SessionIdle` (emitted by the daemon at delegate turn-end);
//! tool-permission requests surface as `ApprovalRequested` → the approval modal
//! (`session.respond`). Styling is a Codex-inspired native desktop surface.

use crate::events::route_event;
use crate::render::render_turn;
use crate::rpc::{cancel_job, daemon_token, open_events, start_job, start_job_get_id};
use leptos::prelude::*;
use serde_json::json;

const DEFAULT_AGENT: &str = "claude";

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

/// One conversation in the sidebar list (in-memory for this browser session).
/// `session` is the delegate session id (the `delegate.start` job id); `turn_job`
/// is the in-flight turn's job id (start or steer), used to cancel/stop it.
#[derive(Clone)]
pub(crate) struct Chat {
    pub(crate) title: String,
    pub(crate) session: Option<String>,
    pub(crate) turn_job: Option<String>,
    pub(crate) turns: Vec<Turn>,
    pub(crate) streaming: bool,
}

impl Chat {
    fn new() -> Self {
        Self {
            title: "New thread".into(),
            session: None,
            turn_job: None,
            turns: Vec::new(),
            streaming: false,
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

#[component]
pub fn App() -> impl IntoView {
    let token = StoredValue::new(daemon_token());
    let chats = RwSignal::new(vec![Chat::new()]);
    let active = RwSignal::new(0usize);
    let input = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let approval = RwSignal::new(None::<ApprovalReq>);
    // The local CLI to drive (claude / codex / gemini) + the autonomy posture
    // passed to delegate.start (full = no prompts, edit, read_only = plan).
    let agent = RwSignal::new(DEFAULT_AGENT.to_string());
    let autonomy = RwSignal::new("full".to_string());
    let inspector_open = RwSignal::new(false);
    let workspace = RwSignal::new("workspace".to_string());
    let branch = RwSignal::new("—".to_string());

    Effect::new(move |_| {
        let Some(tok) = token.get_value() else {
            error.set(Some(
                "no daemon token — open the daemon's URL (or append #token=…)".into(),
            ));
            return;
        };
        let _ = open_events(&tok, move |event| route_event(event, chats, approval));
        leptos::task::spawn_local(async move {
            if let Some((name, _root)) = crate::data::fetch_workspace(&tok).await {
                workspace.set(name);
            }
            if let Some(b) = crate::data::fetch_branch(&tok).await {
                branch.set(b);
            }
        });
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
        let existing = chats.with_untracked(|cs| cs.get(idx).and_then(|c| c.session.clone()));
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
        let (ag, au) = (agent.get_untracked(), autonomy.get_untracked());
        leptos::task::spawn_local(async move {
            let outcome = match existing {
                // Follow-up: steer the live delegate session.
                Some(sid) => match start_job_get_id(
                    &tok,
                    json!({"kind": "delegate.steer", "session_id": sid, "message": text}),
                )
                .await
                {
                    Ok(turn_job) => {
                        set_turn_job(chats, idx, turn_job);
                        Ok(())
                    }
                    Err(err) => Err(format!("delegate.steer: {err}")),
                },
                // First message: start a delegate session (task = the message).
                None => start_delegate(&tok, chats, idx, &ag, &au, &text)
                    .await
                    .map(|_| ()),
            };
            if let Err(err) = outcome {
                fail_chat(chats, idx, error, err);
            }
        });
    };

    let stop = move || {
        let Some(tok) = token.get_value() else { return };
        let idx = active.get_untracked();
        let Some(job) = chats.with_untracked(|cs| cs.get(idx).and_then(|c| c.turn_job.clone()))
        else {
            return;
        };
        leptos::task::spawn_local(async move {
            let _ = cancel_job(&tok, &job).await;
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

    // Inspector (Plan/Files/Changes): Files + Changes load real data from the
    // daemon's snapshot-backed tools; Plan shows the active turn's tool calls.
    let inspector_tab = RwSignal::new("changes");
    let inspector_data = RwSignal::new(String::new());
    let load_tab = move |tab: &'static str| {
        inspector_tab.set(tab);
        if tab == "plan" {
            return;
        }
        let Some(tok) = token.get_value() else { return };
        inspector_data.set("loading…".into());
        leptos::task::spawn_local(async move {
            let text = match tab {
                "files" => crate::data::fetch_file_tree(&tok).await,
                "changes" => crate::data::fetch_diff(&tok).await,
                _ => None,
            };
            inspector_data.set(text.unwrap_or_else(|| "—".into()));
        });
    };

    let toggle_inspector = move |_| {
        let opening = !inspector_open.get_untracked();
        inspector_open.set(opening);
        if opening {
            load_tab(inspector_tab.get_untracked());
        }
    };

    // The composer: a large rounded box (textarea + an inline tool row) with
    // context pills beneath. Reused as the centered hero (empty state) and the
    // docked bar (active conversation). Copy closure → usable in both branches.
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
                        placeholder="Ask the agent to build…"
                    ></textarea>
                    <div class="composer-tools">
                        <select
                            class="access-pill"
                            title="Autonomy"
                            prop:value=move || autonomy.get()
                            on:change=move |ev| autonomy.set(event_target_value(&ev))
                        >
                            <option value="full">"Full access"</option>
                            <option value="edit">"Auto-edit"</option>
                            <option value="read_only">"Read-only"</option>
                        </select>
                        <span class="tool-spacer"></span>
                        <span class="effort-model">
                            {move || crate::data::agent_label(&agent.get()).to_string()}
                        </span>
                        {move || if active_busy() {
                            view! { <button class="send stop" title="Stop" on:click=move |_| stop()>"■"</button> }.into_any()
                        } else {
                            view! { <button class="send" title="Send" on:click=move |_| send()>"↑"</button> }.into_any()
                        }}
                    </div>
                </div>
                <div class="context-pills">
                    <span class="ctx-pill">"⌂ "{move || workspace.get()}</span>
                    <span class="ctx-pill">{move || crate::data::agent_label(&agent.get()).to_string()}</span>
                    <span class="ctx-pill">"⎇ "{move || branch.get()}</span>
                </div>
            </div>
        }
    };

    // Only flips when the active chat goes empty↔non-empty, so the composer is
    // not re-created (losing focus) on every streaming delta.
    let empty = Memo::new(move |_| {
        chats.with(|cs| {
            cs.get(active.get())
                .map(|c| c.turns.is_empty())
                .unwrap_or(true)
        })
    });

    view! {
        <div id="nerve-shell" class:with-inspector=move || inspector_open.get()>
            <aside class="sidebar">
                <div class="brand"><span class="spark">"N"</span><span>"Nerve"</span></div>
                <button class="newchat" title="New thread" on:click=new_chat>
                    <span class="plus">"+"</span>"New thread"
                </button>
                <div class="rail-label">"Projects"</div>
                <div class="project-row"><span class="project-dot"></span><span>{move || workspace.get()}</span></div>
                <div class="rail-label">"Conversations"</div>
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
                        <select
                            class="model-pill"
                            title="Agent CLI"
                            prop:value=move || agent.get()
                            on:change=move |ev| agent.set(event_target_value(&ev))
                        >
                            {crate::data::AGENTS.iter().map(|(id, label)| view! {
                                <option value=*id>{*label}</option>
                            }).collect_view()}
                        </select>
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
                        <span class="inspector-title">"Inspector"</span>
                        <span class="inspector-chip">"⎇ "{move || branch.get()}</span>
                    </div>
                    <div class="inspector-tabs">
                        <button class="inspector-tab" class:on=move || inspector_tab.get() == "plan"
                            on:click=move |_| load_tab("plan")>"Plan"</button>
                        <button class="inspector-tab" class:on=move || inspector_tab.get() == "files"
                            on:click=move |_| load_tab("files")>"Files"</button>
                        <button class="inspector-tab" class:on=move || inspector_tab.get() == "changes"
                            on:click=move |_| load_tab("changes")>"Changes"</button>
                    </div>
                    <div class="inspector-body">
                        {move || if inspector_tab.get() == "plan" {
                            let tools = chats.with(|cs| cs.get(active.get())
                                .and_then(|c| c.turns.last().map(|t| t.tools.clone()))
                                .unwrap_or_default());
                            if tools.is_empty() {
                                view! { <div class="plan-empty">"No tool activity in this thread yet."</div> }.into_any()
                            } else {
                                tools.into_iter().map(|t| view! {
                                    <div class="plan-step done"><span></span><p>{t.tool}</p></div>
                                }).collect_view().into_any()
                            }
                        } else {
                            view! { <pre class="inspector-pre">{move || inspector_data.get()}</pre> }.into_any()
                        }}
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

/// Send a `session.respond` decision (delegate approvals share the same hub) and
/// clear the modal.
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

/// Start a delegate session for the chat: `delegate.start` carries the first
/// message as the task; its job id becomes the session id (and the turn-1 job).
async fn start_delegate(
    token: &str,
    chats: RwSignal<Vec<Chat>>,
    idx: usize,
    agent: &str,
    autonomy: &str,
    task: &str,
) -> Result<String, String> {
    let cmd = json!({
        "kind": "delegate.start",
        "agent": agent,
        "task": task,
        "autonomy": autonomy,
    });
    let id = start_job_get_id(token, cmd)
        .await
        .map_err(|err| format!("delegate.start: {err}"))?;
    chats.update(|cs| {
        if let Some(c) = cs.get_mut(idx) {
            c.session = Some(id.clone());
            c.turn_job = Some(id.clone());
        }
    });
    Ok(id)
}

/// Record the in-flight turn's job id (for Stop / cancellation).
fn set_turn_job(chats: RwSignal<Vec<Chat>>, idx: usize, job: String) {
    chats.update(|cs| {
        if let Some(c) = cs.get_mut(idx) {
            c.turn_job = Some(job);
        }
    });
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
            c.turn_job = None;
            if let Some(turn) = c.turns.last_mut() {
                turn.streaming = false;
            }
        }
    });
    error.set(Some(message));
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
