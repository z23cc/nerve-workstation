//! The WeChat (个人微信) bridge panel: QR login + bridge config/control + a live
//! status and message log driven by folding `RuntimeEvent::Wechat` SSE events.
//!
//! The panel is a Protocol-v4 client like the rest of the GUI: `Log in` runs a
//! `wechat.login` job (the daemon streams the QR + login transitions back as
//! global `wechat` events, folded by [`crate::events::route_event`]), and
//! `Start`/`Stop` run `wechat.start` / `wechat.stop`. `wechat.start` requires the
//! daemon to have been launched with `--allow-delegate` AND a prior successful
//! login — the daemon's error for either is surfaced verbatim in the status line.
//!
//! Mirrors `settings_auth.rs`: a `#[component]` owning local request-busy signals,
//! a `StoredValue<Option<String>>` token prop, and `spawn_local` blocks calling
//! [`crate::rpc::start_job_await`]. Split out of `app.rs` to stay under the
//! file-size gate.

// View-heavy component; see app.rs for why too_many_lines is allowed at module
// scope (the `#[component]` macro drops a fn-level allow).
#![allow(clippy::too_many_lines)]

use crate::rpc::start_job_await;
use leptos::prelude::*;
use nerve_proto::WechatEventKind;
use serde_json::json;

const NO_TOKEN: &str = "No daemon token — open the daemon URL or append #token=…";
/// Cap on the live message log so a long-running bridge does not grow unbounded.
const LOG_CAP: usize = 200;
const AGENT_OPTS: &[(&str, &str)] = &[
    ("claude", "Claude Code"),
    ("codex", "Codex"),
    ("gemini", "Gemini CLI"),
];
const AUTO_OPTS: &[(&str, &str)] = &[
    ("read_only", "Read-only"),
    ("edit", "Auto-edit"),
    ("full", "Full access"),
];

/// The reactive state of the WeChat panel, owned by `app.rs` and threaded into
/// [`crate::events::route_event`] so the SSE fold updates the same signals the
/// panel renders. `Copy` (all fields are `RwSignal`) so it passes by value.
#[derive(Clone, Copy)]
pub(crate) struct WeChatSignals {
    /// Whether the panel overlay is open.
    pub(crate) open: RwSignal<bool>,
    /// The remote HTTPS QR image URL to render (`""` = no QR to show).
    pub(crate) qr_url: RwSignal<String>,
    /// Human-readable login status line.
    pub(crate) qr_status: RwSignal<String>,
    /// Whether the bridge is running (drives the status pill + Start/Stop).
    pub(crate) running: RwSignal<bool>,
    /// The live relayed-message log (newest last), capped at [`LOG_CAP`].
    pub(crate) log: RwSignal<Vec<String>>,
}

impl WeChatSignals {
    pub(crate) fn new() -> Self {
        Self {
            open: RwSignal::new(false),
            qr_url: RwSignal::new(String::new()),
            qr_status: RwSignal::new("Not logged in.".to_string()),
            running: RwSignal::new(false),
            log: RwSignal::new(Vec::new()),
        }
    }
}

/// Fold one `WechatEventKind` (from a global `RuntimeEvent::Wechat`) into the
/// panel signals. Pure routing — every case maps to a reactive update.
pub(crate) fn fold_wechat_event(kind: WechatEventKind, w: WeChatSignals) {
    match kind {
        WechatEventKind::LoginQr { image_url, .. } => {
            w.qr_url.set(image_url);
            w.qr_status.set("Waiting for scan…".into());
        }
        WechatEventKind::LoginStatus { status } => w.qr_status.set(status),
        WechatEventKind::LoggedIn { .. } => {
            w.qr_url.set(String::new());
            w.qr_status.set("Logged in".into());
        }
        WechatEventKind::LoginFailed { error } => {
            w.qr_url.set(String::new());
            w.qr_status.set(format!("Login failed: {error}"));
        }
        WechatEventKind::BridgeStatus { running, .. } => w.running.set(running),
        WechatEventKind::Message {
            direction, text, ..
        } => w.log.update(|log| push_log(log, &direction, &text)),
    }
}

/// Append a `"<direction>: <text>"` line to the log, dropping the oldest once the
/// log reaches [`LOG_CAP`] so the live feed stays bounded.
fn push_log(log: &mut Vec<String>, direction: &str, text: &str) {
    log.push(format!("{direction}: {text}"));
    if log.len() > LOG_CAP {
        let overflow = log.len() - LOG_CAP;
        log.drain(0..overflow);
    }
}

#[component]
pub(crate) fn WeChatPanel(
    token: StoredValue<Option<String>>,
    wechat: WeChatSignals,
) -> impl IntoView {
    let bot_type = RwSignal::new(String::new());
    let owners = RwSignal::new(String::new());
    let agent = RwSignal::new("claude".to_string());
    let autonomy = RwSignal::new("read_only".to_string());
    let login_busy = RwSignal::new(false);
    let bridge_busy = RwSignal::new(false);

    let close = move || wechat.open.set(false);
    let log_in = move || request_login(token, bot_type, wechat.qr_status, login_busy);
    let start = move || {
        request_start(StartArgs {
            token,
            owners,
            agent,
            autonomy,
            status: wechat.qr_status,
            busy: bridge_busy,
        });
    };
    let stop = move || request_simple(token, "wechat.stop", wechat.qr_status, bridge_busy);

    view! {
        <div class="modal-scrim" hidden=move || !wechat.open.get() on:click=move |_| close()>
            <div id="wechat-dialog" class="modal wechat-panel" role="dialog" aria-modal="true"
                aria-labelledby="wechat-title" aria-describedby="wechat-status" tabindex="-1"
                on:click=move |ev| ev.stop_propagation()>
                <div class="modal-head">
                    <span id="wechat-title" class="modal-title">"WeChat bridge"</span>
                    <span class=move || if wechat.running.get() { "tier full" } else { "tier read_only" }>
                        {move || if wechat.running.get() { "running" } else { "stopped" }}
                    </span>
                    <button type="button" class="cmd-close" title="Close" aria-label="Close WeChat panel"
                        aria-keyshortcuts="Escape" on:click=move |_| close()>"Esc"</button>
                </div>
                <p class="set-desc">
                    "Log in by scanning the QR with WeChat, then start the bridge. \
                     Starting needs the daemon launched with --allow-delegate and a prior login."
                </p>
                <div class="wechat-login">
                    <label class="wechat-field">
                        <span class="set-desc">"Bot type"</span>
                        <input id="wechat-bot-type" name="wechat-bot-type" class="set-input wechat-bot-type"
                            type="text" spellcheck="false" placeholder="your iLink bot type"
                            aria-label="iLink bot type"
                            prop:value=move || bot_type.get()
                            disabled=move || login_busy.get()
                            on:input=move |ev| bot_type.set(event_target_value(&ev)) />
                    </label>
                    <button class="btn allow" type="button"
                        disabled=move || login_busy.get() || bot_type.get().trim().is_empty()
                        aria-label="Log in to WeChat with the given bot type"
                        aria-controls="wechat-status"
                        on:click=move |_| log_in()>"Log in"</button>
                </div>
                {move || (!wechat.qr_url.get().is_empty()).then(|| view! {
                    <img class="wechat-qr" alt="WeChat login QR code — scan with WeChat"
                        src=move || wechat.qr_url.get() />
                })}
                <pre id="wechat-status" class="lease-status wechat-status" role="status" aria-live="polite">
                    {move || wechat.qr_status.get()}
                </pre>
                <hr class="set-div"/>
                <label class="wechat-field">
                    <span class="set-desc">"Owners — one WeChat user id per line (empty denies everyone)"</span>
                    <textarea id="wechat-owners" name="wechat-owners" class="set-input wechat-owners"
                        rows="3" spellcheck="false" placeholder="wxid_one\nwxid_two"
                        aria-label="Allowed WeChat owner user ids, one per line"
                        prop:value=move || owners.get()
                        disabled=move || bridge_busy.get()
                        on:input=move |ev| owners.set(event_target_value(&ev))></textarea>
                </label>
                <div class="wechat-config">
                    <label class="wechat-field">
                        <span class="set-desc">"Agent"</span>
                        <select id="wechat-agent" name="wechat-agent" class="set-select"
                            prop:value=move || agent.get() aria-label="Delegate agent"
                            disabled=move || bridge_busy.get()
                            on:change=move |ev| agent.set(event_target_value(&ev))>
                            {AGENT_OPTS.iter().map(|(id, label)| view! { <option value=*id>{*label}</option> }).collect_view()}
                        </select>
                    </label>
                    <label class="wechat-field">
                        <span class="set-desc">"Autonomy"</span>
                        <select id="wechat-autonomy" name="wechat-autonomy" class="set-select"
                            prop:value=move || autonomy.get() aria-label="Delegate autonomy"
                            disabled=move || bridge_busy.get()
                            on:change=move |ev| autonomy.set(event_target_value(&ev))>
                            {AUTO_OPTS.iter().map(|(id, label)| view! { <option value=*id>{*label}</option> }).collect_view()}
                        </select>
                    </label>
                </div>
                <div class="modal-actions">
                    <button type="button" class="btn allow"
                        disabled=move || bridge_busy.get() || wechat.running.get()
                        aria-label="Start the WeChat bridge"
                        on:click=move |_| start()>"Start"</button>
                    <button type="button" class="btn danger"
                        disabled=move || bridge_busy.get() || !wechat.running.get()
                        aria-label="Stop the WeChat bridge"
                        on:click=move |_| stop()>"Stop"</button>
                </div>
                <hr class="set-div"/>
                <div class="wechat-log-head set-desc">"Message log"</div>
                <div class="wechat-log" role="log" aria-label="WeChat message log" aria-live="polite">
                    {move || {
                        let log = wechat.log.get();
                        if log.is_empty() {
                            view! { <div class="wechat-log-empty">"No messages relayed yet."</div> }.into_any()
                        } else {
                            log.into_iter()
                                .map(|line| view! { <div class="wechat-log-line">{line}</div> })
                                .collect_view()
                                .into_any()
                        }
                    }}
                </div>
            </div>
        </div>
    }
}

/// Run a `wechat.login` job: it streams the QR + login transitions back as global
/// `wechat` events (folded by `route_event`), so success here just clears the busy
/// flag — the QR appears via the event fold.
fn request_login(
    token: StoredValue<Option<String>>,
    bot_type: RwSignal<String>,
    status: RwSignal<String>,
    busy: RwSignal<bool>,
) {
    let Some(tok) = token.get_value() else {
        status.set(NO_TOKEN.into());
        return;
    };
    if busy.get_untracked() {
        return;
    }
    let bot = bot_type.get_untracked().trim().to_string();
    if bot.is_empty() {
        status.set("Enter a bot type first.".into());
        return;
    }
    busy.set(true);
    status.set("Requesting WeChat login QR…".into());
    leptos::task::spawn_local(async move {
        let cmd = json!({ "kind": "wechat.login", "bot_type": bot });
        if let Err(err) = start_job_await(&tok, cmd).await {
            status.set(format!("Login failed: {err}"));
        }
        busy.set(false);
    });
}

/// Bundled `wechat.start` arguments (clippy `too_many_arguments`).
struct StartArgs {
    token: StoredValue<Option<String>>,
    owners: RwSignal<String>,
    agent: RwSignal<String>,
    autonomy: RwSignal<String>,
    status: RwSignal<String>,
    busy: RwSignal<bool>,
}

/// Run `wechat.start`. Surfaces the daemon's error verbatim (e.g. delegate not
/// allowed, or no prior login).
fn request_start(args: StartArgs) {
    let Some(tok) = args.token.get_value() else {
        args.status.set(NO_TOKEN.into());
        return;
    };
    if args.busy.get_untracked() {
        return;
    }
    let owners: Vec<String> = args
        .owners
        .get_untracked()
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    let (agent, autonomy) = (args.agent.get_untracked(), args.autonomy.get_untracked());
    args.busy.set(true);
    args.status.set("Starting WeChat bridge…".into());
    let status = args.status;
    let busy = args.busy;
    leptos::task::spawn_local(async move {
        let cmd = json!({
            "kind": "wechat.start",
            "owners": owners,
            "agent": agent,
            "autonomy": autonomy,
        });
        match start_job_await(&tok, cmd).await {
            Ok(_) => status.set("Bridge start requested.".into()),
            Err(err) => status.set(format!("Start failed: {err}")),
        }
        busy.set(false);
    });
}

/// Run a unit-variant command (`wechat.stop`), reporting into `status`.
fn request_simple(
    token: StoredValue<Option<String>>,
    kind: &'static str,
    status: RwSignal<String>,
    busy: RwSignal<bool>,
) {
    let Some(tok) = token.get_value() else {
        status.set(NO_TOKEN.into());
        return;
    };
    if busy.get_untracked() {
        return;
    }
    busy.set(true);
    status.set("Stopping WeChat bridge…".into());
    leptos::task::spawn_local(async move {
        match start_job_await(&tok, json!({ "kind": kind })).await {
            Ok(_) => status.set("Bridge stop requested.".into()),
            Err(err) => status.set(format!("Stop failed: {err}")),
        }
        busy.set(false);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_log_caps_and_drops_oldest() {
        let mut log = Vec::new();
        for i in 0..(LOG_CAP + 5) {
            push_log(&mut log, "in", &format!("m{i}"));
        }
        assert_eq!(log.len(), LOG_CAP);
        // Oldest five dropped: the first surviving line is message #5.
        assert_eq!(log.first().unwrap(), "in: m5");
        assert_eq!(log.last().unwrap(), &format!("in: m{}", LOG_CAP + 4));
    }

    #[test]
    fn push_log_formats_direction_and_text() {
        let mut log = Vec::new();
        push_log(&mut log, "out", "hi there");
        assert_eq!(log[0], "out: hi there");
    }
}
