//! The ratatui shell: connect → handshake → `session.start`, then a
//! `tokio::select!` loop multiplexing keyboard input, protocol events, and a
//! tick. Keys are dispatched in [`input`]; events are folded in [`events`]; the
//! frame is composed purely in [`render`].
//!
//! The interactive LLM path needs provider credentials, so it is exercised by
//! hand, not in CI; the protocol client, the render path, and the key/command
//! reductions are what the tests cover. The approval modal (`Mode::Approval`)
//! key path is stubbed here (T4 fills it on top of the dispatch + `ApprovalState`
//! hook this wave lands).

mod events;
mod input;
pub mod render;
pub mod state;
mod terminal;

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyEventKind, MouseEventKind};
use futures::StreamExt;
use nerve_runtime::{RuntimeCommand, RuntimeEvent};
use tokio::sync::broadcast;

use crate::protocol::{DaemonSpec, NerveClient};
use events::apply_event;
use state::State;
use terminal::TerminalGuard;

/// Run the interactive shell against a daemon spawned from `spec`, starting a
/// session with `provider`/`model`.
pub async fn run(spec: DaemonSpec, provider: String, model: String) -> Result<()> {
    let (client, events) = NerveClient::connect(spec).await?;
    let mut shell = Shell::new(client, events, State::new(provider.clone(), model.clone()));
    shell.startup(provider, model).await;
    let result = shell.event_loop().await;
    shell.client.shutdown().await;
    result
}

pub(crate) struct Shell {
    pub(crate) client: NerveClient,
    events: broadcast::Receiver<RuntimeEvent>,
    pub(crate) state: State,
}

impl Shell {
    fn new(client: NerveClient, events: broadcast::Receiver<RuntimeEvent>, state: State) -> Self {
        Self {
            client,
            events,
            state,
        }
    }

    /// Populate the tool count and open the session.
    async fn startup(&mut self, provider: String, model: String) {
        self.state.tools = self.client.list_tools().await.map(|t| t.len()).unwrap_or(0);
        self.state.note(format!(
            "connected · {} tools · type a message · /help for commands",
            self.state.tools
        ));
        if let Err(err) = self
            .client
            .start_job(Self::session_start_command(provider, model), None)
            .await
        {
            self.state.note(format!("session.start failed: {err}"));
        }
    }

    /// Build a `session.start` command for the current provider/model.
    pub(crate) fn session_start_command(provider: String, model: String) -> RuntimeCommand {
        RuntimeCommand::SessionStart {
            workspace: None,
            provider,
            model,
            system_prompt: None,
            agent: None,
            resume: None,
            max_turns: None,
            temperature: None,
            reasoning_effort: None,
            tool_filter: None,
        }
    }

    /// The main multiplexed loop. Returns when the user quits.
    async fn event_loop(&mut self) -> Result<()> {
        let mut guard = TerminalGuard::enter()?;
        let mut keys = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(90));
        self.draw(&mut guard)?;
        loop {
            let mut dirty = false;
            tokio::select! {
                maybe_key = keys.next() => match maybe_key {
                    Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                        if self.handle_key(key).await {
                            return Ok(());
                        }
                        dirty = true;
                    }
                    Some(Ok(Event::Resize(_, _))) => dirty = true,
                    Some(Ok(Event::Paste(text))) => {
                        self.handle_paste(&text);
                        dirty = true;
                    }
                    Some(Ok(Event::Mouse(mouse))) => match mouse.kind {
                        MouseEventKind::ScrollUp => { self.state.scroll += 3; dirty = true; }
                        MouseEventKind::ScrollDown => {
                            self.state.scroll = self.state.scroll.saturating_sub(3);
                            dirty = true;
                        }
                        _ => {}
                    },
                    Some(Err(_)) | None => return Ok(()),
                    _ => {}
                },
                event = self.events.recv() => if let Some(redraw) = self.on_event(event) {
                    dirty = redraw;
                },
                _ = tick.tick() => if self.state.running {
                    self.state.tick_spinner();
                    if let Some(started) = self.state.turn_started_at {
                        self.state.elapsed_ms = started.elapsed().as_millis() as u64;
                    }
                    dirty = true;
                },
            }
            if dirty {
                self.draw(&mut guard)?;
            }
        }
    }

    /// Fold one broadcast result into state. `None` means the stream closed and
    /// nothing changed; `Some(redraw)` reports whether to re-render.
    fn on_event(
        &mut self,
        event: Result<RuntimeEvent, broadcast::error::RecvError>,
    ) -> Option<bool> {
        match event {
            Ok(event) => Some(apply_event(&mut self.state, &event)),
            Err(broadcast::error::RecvError::Lagged(_)) => Some(false),
            Err(broadcast::error::RecvError::Closed) => {
                self.state.note("daemon disconnected");
                Some(true)
            }
        }
    }

    fn draw(&mut self, guard: &mut TerminalGuard) -> Result<()> {
        guard
            .terminal
            .draw(|frame| render::render(frame, &self.state))?;
        Ok(())
    }
}
