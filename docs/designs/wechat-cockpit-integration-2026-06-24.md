# WeChat cockpit integration — 2026-06-24

Wire the personal-WeChat (个人微信) "clawbot" into the **GUI and TUI** by hosting the
bridge **inside `nerve daemon`** and exposing it over the **versioned runtime
protocol** — the north-star rule for any new client surface (clients talk to the
daemon over the protocol, never a bespoke path). This replaces the standalone
`nerve-wechat` binary's topology (which spawned its *own* child `nerve daemon`).

## Before vs after

- **Before:** `crates/nerve-wechat` was a complete standalone binary (QR login,
  fail-closed owner allowlist, long-poll bridge, session persistence) that drove a
  *child* `nerve daemon --stdio` over NDJSON. Neither the GUI nor the TUI had any
  WeChat surface.
- **After:** `nerve daemon` hosts the bridge in-process and drives its **own**
  delegate machinery; the GUI and TUI control login + the bridge over `wechat.*`
  protocol commands and render `wechat` events. The standalone binary still works
  unchanged (its `DelegateNerve` child-daemon path is untouched).

## Layers landed (all green: clippy -D · fmt · file-size · tests · protocol drift)

### 1. Protocol (`nerve-proto`, drift-checked, schema regenerated)

Commands (`RuntimeCommand`, serde tag `kind`):

| kind | fields | meaning |
|---|---|---|
| `wechat.login` | `bot_type: String`, `base_url: Option<String>` | run the QR login flow as a cancellable **job**; stream `wechat` events; cache the confirmed session |
| `wechat.start` | `owners: Vec<String>`, `agent: String` (default `claude`), `autonomy: DelegateAutonomy` (default `read_only`) | start the long-poll bridge against the logged-in session; each allowed owner's message drives one `delegate.*` turn |
| `wechat.stop` | — | stop the bridge (idempotent) |
| `wechat.status` | — | report login + bridge state |

Events: one new `RuntimeEvent::Wechat { kind: WechatEventKind }` (tag `type` =
`wechat`), with nested `WechatEventKind` (tag `kind`, snake_case):
`login_qr {qrcode, image_url}` · `login_status {status}` ·
`logged_in {account_id, user_id}` · `login_failed {error}` ·
`bridge_status {running, account_id, user_id}` ·
`message {chat_key, from_user_id, direction, text}`.

WeChat events are **global/unscoped** — `RuntimeEvent::session_id()` returns `None`,
so the per-id fan-out delivers them to **every** connected client (like `Auth`). Any
GUI/TUI surface renders login + bridge status live without owning a session.

`bot_type` is **required** for login and is **not a published constant** (it comes
from your iLink bot registration) — the caller supplies it; this is the value you
enter when logging in to test.

### 2. Daemon hosting (`nerve-workstation`)

- `crates/nerve-workstation/src/wechat/` — a new module:
  - `control.rs`: `RuntimeNerve` implements `nerve_wechat::NerveControl` by running a
    **one-shot** delegate turn against the daemon's in-process launcher (the
    `run_delegate_oneshot` free fn mirrors `JobManager::run_delegate`), returning the
    agent's final text. Lives here (not in `nerve-wechat`) because it needs the
    workstation's delegate launcher, and `nerve-wechat` must not depend on
    `nerve-workstation` (cycle). Inbound/outbound text is mirrored as `message`
    events for a live log.
  - `mod.rs`: `WechatHost` owns the logged-in `WeixinSession` + the bridge thread.
    `login` runs the cancellable QR poll on the calling job thread; `start` spawns the
    stop-checked bridge on its **own** `std::thread` (the gateway is blocking `ureq`
    ~40s long-poll, so it must never run on a dispatch thread); `stop` signals + joins;
    `status` reports state.
- `jobs.rs`: a new `Executor::Wechat`, routed by the exhaustive `executor_for` (the
  §10 compile-time totality gate) and `run_wechat_command`. `WechatHost` is a
  `JobManager` field built from the shared event emitter, so it reaches **both**
  transports (stdio + HTTP/SSE) and broadcasts to all clients with zero transport
  change. `wechat.start` is gated on `--allow-delegate` + a served `--root`.
- `nerve-wechat`: added `Bridge::run_until(should_stop)` (a stop-aware variant of the
  long-poll loop) so the host can cleanly stop the bridge thread.

### 3. TUI (`nerve-tui`)

- `app/input/wechat.rs` (new): `/wechat login <bot_type> [base_url]`, `/wechat start
  [agent] [autonomy] [owner1,owner2,…]`, `/wechat stop`, `/wechat status` — pure
  command builders + `Shell` handlers, mirroring `delegate.rs`. Dispatched from the
  `on_command` arm in `app/input/mod.rs`; registered in `ui/commands.rs` (palette +
  HELP_TEXT).
- `app/state.rs`: a `Block::WechatBridge` variant + helpers (`wechat_set_status`,
  `wechat_set_qr`, `wechat_push_message`, 50-message cap). `app/events.rs`:
  `apply_wechat_event` folds all six `WechatEventKind` arms.
- `ui/wechat_render.rs` (new): renders the bridge block — header, the QR as a
  **text URL + id** (ratatui has no inline-image/OSC-8 cell support), and the rolling
  in/out message log. Pinned by an insta snapshot.

### 4. GUI (`nerve-gui`, Leptos wasm CSR)

- `wechat_panel.rs` (new): a `WeChatPanel` modal — a `bot_type` input + **Log in**
  (runs `wechat.login`), a direct `<img src=…>` QR render (the gateway image is a
  remote HTTPS URL, so the markdown `data:` sanitizer doesn't apply), a live status
  line, an owners textarea + agent/autonomy selects, and **Start/Stop** buttons
  (`wechat.start` / `wechat.stop`, daemon errors surfaced verbatim). Commands go
  through `rpc::start_job_await`, mirroring `settings_auth.rs`.
- `events.rs`: `route_event` folds `RuntimeEvent::Wechat { kind }` into a `Copy`
  `WeChatSignals` bundle (qr_url / qr_status / running / log, 200-line cap).
- Surfaced via `command_catalog.rs` (⌘K), `command_palette.rs`, and a `sidebar.rs`
  nav button. The committed `dist/` (`index.html`, `nerve-gui.js`,
  `nerve-gui_bg.wasm`, `styles.css`) was rebuilt with `trunk build` so the panel
  ships (the daemon `include_bytes!`s it); the daemon `daemon/app.rs` dist-sync tests
  stay green.

All gates green together: `cargo test --workspace` (1384 passed, 0 failed),
`cargo clippy --workspace --all-targets -D warnings`, `cargo clippy -p nerve-gui
--target wasm32-unknown-unknown -D warnings`, `cargo fmt --all --check`,
`./Scripts/check-file-size.sh`, the protocol drift test, and an end-to-end stdio
smoke (`wechat.status` → `job_started`/`job_completed`).

## How to log in and test

1. Build + run the daemon against your project, **with delegation lifted**:
   `nerve daemon --root /abs/project --allow-delegate` (HTTP for the GUI, or the
   TUI/`nerve chat` for the stdio path).
2. From the GUI panel or TUI `/wechat login <bot_type>`: scan the QR that appears.
3. `wechat.start` (GUI button / TUI `/wechat start`): the bridge runs.
4. From an allowed WeChat owner id, message the bot; the delegated agent (claude by
   default, read-only) replies in the chat. Watch the live log + status on either
   surface.

## Known limitations / follow-ups

- **One-shot turns:** each owner message starts a fresh delegate run (no
  cross-message conversation continuity yet). Reusing the live-session/steer
  machinery is the natural next step.
- **Session persistence:** the logged-in session lives in the daemon process; a
  daemon restart needs a re-scan. (The standalone binary persists to disk; porting
  `session_store` to a daemon-scoped path is a small follow-up.)
- **Stop latency:** an in-flight ~40s long-poll delays `wechat.stop` until it
  returns.
- **Message attribution:** the live log's `message` events carry an empty
  `from_user_id` at the in-process `NerveControl` layer (the allowlist already gated
  the sender); richer attribution would thread the inbound `WeixinMessage` through.
- **Media:** text-only (image/file relay remains the documented deferral).
