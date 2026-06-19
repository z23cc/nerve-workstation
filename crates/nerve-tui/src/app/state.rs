//! Pure UI state + frame rendering for the shell.
//!
//! The render path is a pure function of [`State`] → ratatui widgets, so it is
//! testable against a `TestBackend` with no terminal. T2 replaces the minimal
//! transcript draw with the rich [`crate::ui`] renderer (markdown / syntax
//! highlight / diff / framed tool cells); the streaming-coalesce reduction
//! mirrors the TS `app.ts` (`#appendText` / `#finishTool`).

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Borders, Paragraph, Wrap};

pub use crate::ui::render::ToolCall;
use crate::ui::render::{self, RenderOptions, SPINNER};

/// Severity tone of a client-side notice (drives its color). Ports the TS
/// notice `tone` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Warn,
    Error,
}

/// Lifecycle status of a tool call. Ports the TS tool `status` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Ok,
    Error,
}

/// One rendered transcript entry — the full block set (T2). Assistant text is
/// markdown; reasoning is the dim `·`-gutter stream; a tool call carries its
/// framed status cell; a notice carries a tone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A line the user submitted.
    User(String),
    /// Streaming assistant markdown (appended in place as deltas arrive).
    Assistant(String),
    /// Streaming reasoning text (dim, `·` gutter).
    Reasoning(String),
    /// A tool call cell (running → ok/error, with framed output).
    Tool(ToolCall),
    /// A client-side notice (connection status, errors, hints).
    Notice { tone: Tone, text: String },
}

/// The whole shell state. Mutated by the event loop; rendered purely.
#[derive(Debug, Clone)]
pub struct State {
    pub provider: String,
    pub model: String,
    pub tools: usize,
    pub blocks: Vec<Block>,
    pub input: String,
    /// True while a turn is in flight (drives the status line).
    pub running: bool,
    /// One-shot status hint (e.g. "interrupting…"); cleared on next input.
    pub hint: String,
    pub session_id: Option<String>,
    /// Spinner frame, advanced on each tick while running.
    pub spinner: usize,
    /// Expand tool cells (show full capped output instead of a 3-line preview).
    pub expand_tools: bool,
    /// Index of the assistant block currently being streamed into, if any.
    assistant: Option<usize>,
    /// Index of the reasoning block currently being streamed into, if any.
    reasoning: Option<usize>,
}

impl State {
    #[must_use]
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            tools: 0,
            blocks: Vec::new(),
            input: String::new(),
            running: false,
            hint: String::new(),
            session_id: None,
            spinner: 0,
            expand_tools: false,
            assistant: None,
            reasoning: None,
        }
    }

    /// Push an info-tone notice (connection status, hints).
    pub fn note(&mut self, text: impl Into<String>) {
        self.push_notice(Tone::Info, text);
    }

    /// Push a notice with an explicit tone (warn/error).
    pub fn push_notice(&mut self, tone: Tone, text: impl Into<String>) {
        self.blocks.push(Block::Notice {
            tone,
            text: text.into(),
        });
        self.end_stream();
    }

    /// Push a user message block.
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.blocks.push(Block::User(text.into()));
        self.end_stream();
    }

    /// Append a streaming assistant delta, coalescing into the current assistant
    /// block. Empty deltas are dropped (mirrors TS `#appendText` + the empty-skip
    /// upstream). Starting an assistant run ends any open reasoning run.
    pub fn append_assistant(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if let Some(index) = self.assistant
            && let Some(Block::Assistant(text)) = self.blocks.get_mut(index)
        {
            text.push_str(delta);
            return;
        }
        self.blocks.push(Block::Assistant(delta.to_string()));
        self.assistant = Some(self.blocks.len() - 1);
        self.reasoning = None;
    }

    /// Append a streaming reasoning delta, coalescing into the current reasoning
    /// block. Empty deltas dropped. Starting a reasoning run ends the assistant.
    pub fn append_reasoning(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if let Some(index) = self.reasoning
            && let Some(Block::Reasoning(text)) = self.blocks.get_mut(index)
        {
            text.push_str(delta);
            return;
        }
        self.blocks.push(Block::Reasoning(delta.to_string()));
        self.reasoning = Some(self.blocks.len() - 1);
        self.assistant = None;
    }

    /// Start a tool cell (status: running). Ends the open text stream first, as
    /// the TS does on `tool_started`. Stamps the start time for the duration.
    pub fn start_tool(&mut self, tool: impl Into<String>, args: impl Into<String>) {
        self.end_stream();
        self.blocks.push(Block::Tool(ToolCall {
            tool: tool.into(),
            args: args.into(),
            status: ToolStatus::Running,
            output: None,
            duration_ms: None,
            collapsed: true,
            started_at: Some(std::time::Instant::now()),
        }));
    }

    /// Finish the most recent running cell for `tool`, recording its result and
    /// the elapsed duration. Mirrors TS `#finishTool` (search backwards for a
    /// matching running cell). Used in production; tests use `finish_tool_with`.
    pub fn finish_tool(&mut self, tool: &str, ok: bool, output: impl Into<String>) {
        let elapsed = self
            .running_tool(tool)
            .and_then(|cell| cell.started_at)
            .map_or(0, |started| started.elapsed().as_millis() as u64);
        self.finish_tool_with(tool, ok, output, elapsed);
    }

    /// Like [`finish_tool`](Self::finish_tool) but with an explicit duration, so
    /// the reduction is deterministic in tests (no wall-clock).
    pub fn finish_tool_with(
        &mut self,
        tool: &str,
        ok: bool,
        output: impl Into<String>,
        duration_ms: u64,
    ) {
        for block in self.blocks.iter_mut().rev() {
            if let Block::Tool(cell) = block
                && cell.status == ToolStatus::Running
                && cell.tool == tool
            {
                cell.status = if ok {
                    ToolStatus::Ok
                } else {
                    ToolStatus::Error
                };
                cell.output = Some(output.into());
                cell.duration_ms = Some(duration_ms);
                return;
            }
        }
    }

    /// The most recent still-running cell for `tool`, if any.
    fn running_tool(&self, tool: &str) -> Option<&ToolCall> {
        self.blocks.iter().rev().find_map(|b| match b {
            Block::Tool(cell) if cell.status == ToolStatus::Running && cell.tool == tool => {
                Some(cell)
            }
            _ => None,
        })
    }

    /// End both streaming runs so the next delta starts a fresh block.
    pub fn end_stream(&mut self) {
        self.assistant = None;
        self.reasoning = None;
    }

    /// Advance the spinner frame (called on each tick while running).
    pub fn tick_spinner(&mut self) {
        self.spinner = self.spinner.wrapping_add(1) % SPINNER.len();
    }
}

/// Render the whole frame. Pure w.r.t. `state`; safe to drive from a
/// `TestBackend`. Layout: header / transcript / status / input.
pub fn render(frame: &mut Frame, state: &State) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .split(area);
    frame.render_widget(header(state), chunks[0]);
    render_transcript(frame, state, chunks[1]);
    frame.render_widget(status(state), chunks[2]);
    frame.render_widget(input(state), chunks[3]);
}

fn header(state: &State) -> Paragraph<'_> {
    let text = format!(
        " Nerve  {}/{}  · {} tools",
        state.provider, state.model, state.tools
    );
    Paragraph::new(text).style(Style::default().add_modifier(Modifier::REVERSED))
}

/// Draw the rich transcript: render every block to wrapped styled lines, then
/// top-anchor when the content is shorter than the viewport and bottom-anchor
/// (scroll to the tail) when it overflows — the TS top-anchor fix.
fn render_transcript(frame: &mut Frame, state: &State, area: Rect) {
    let cols = area.width as usize;
    let opts = RenderOptions {
        spinner: state.spinner,
    };
    let mut blocks = state.blocks.clone();
    if state.expand_tools {
        for block in &mut blocks {
            if let Block::Tool(cell) = block {
                cell.collapsed = false;
            }
        }
    }
    let lines: Vec<Line<'static>> = render::blocks_to_lines(&blocks, cols, opts);
    let height = area.height as usize;
    let scroll = lines.len().saturating_sub(height);
    let paragraph = Paragraph::new(lines).scroll((scroll as u16, 0));
    frame.render_widget(paragraph, area);
}

fn status(state: &State) -> Paragraph<'_> {
    let body = if !state.hint.is_empty() {
        state.hint.clone()
    } else if state.running {
        format!(
            "{} working…  Ctrl-C interrupt",
            SPINNER[state.spinner % SPINNER.len()]
        )
    } else {
        "ready  ·  Ctrl-D quit".to_string()
    };
    Paragraph::new(format!(" {body}")).style(Style::default().add_modifier(Modifier::REVERSED))
}

fn input(state: &State) -> Paragraph<'_> {
    // Fully-qualified `ratatui::widgets::Block` here: our transcript `Block` enum
    // shadows the widget name in this module.
    Paragraph::new(format!("❯ {}", state.input))
        .block(ratatui::widgets::Block::default().borders(Borders::TOP))
        .wrap(Wrap { trim: false })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn buffer_text(state: &State, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| render(frame, state)).expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn append_assistant_coalesces_then_splits_on_end() {
        let mut state = State::new("claude", "opus");
        state.append_assistant("Hel");
        state.append_assistant("lo");
        assert_eq!(state.blocks, vec![Block::Assistant("Hello".to_string())]);
        state.end_stream();
        state.append_assistant("World");
        assert_eq!(
            state.blocks,
            vec![
                Block::Assistant("Hello".to_string()),
                Block::Assistant("World".to_string()),
            ]
        );
    }

    #[test]
    fn append_assistant_ignores_empty_delta() {
        let mut state = State::new("claude", "opus");
        state.append_assistant("");
        assert!(state.blocks.is_empty());
    }

    #[test]
    fn reasoning_and_assistant_streams_are_independent() {
        let mut state = State::new("p", "m");
        state.append_reasoning("th");
        state.append_reasoning("ink");
        state.append_assistant("ans"); // ends reasoning run
        state.append_reasoning("more"); // starts a fresh reasoning block
        assert_eq!(
            state.blocks,
            vec![
                Block::Reasoning("think".into()),
                Block::Assistant("ans".into()),
                Block::Reasoning("more".into()),
            ]
        );
    }

    #[test]
    fn tool_lifecycle_running_then_finished() {
        let mut state = State::new("p", "m");
        state.start_tool("read_file", r#"{"path":"a.rs"}"#);
        assert!(matches!(
            state.blocks.last(),
            Some(Block::Tool(c)) if c.status == ToolStatus::Running
        ));
        state.finish_tool_with("read_file", true, "contents", 12);
        let Some(Block::Tool(cell)) = state.blocks.last() else {
            panic!("expected tool block");
        };
        assert_eq!(cell.status, ToolStatus::Ok);
        assert_eq!(cell.output.as_deref(), Some("contents"));
        assert_eq!(cell.duration_ms, Some(12));
    }

    #[test]
    fn render_writes_expected_text_to_test_backend() {
        let mut state = State::new("claude", "opus");
        state.tools = 42;
        state.note("connected");
        state.push_user("hello there");
        state.append_assistant("hi human");
        let text = buffer_text(&state, 60, 12);
        assert!(text.contains("Nerve"), "header missing: {text}");
        assert!(text.contains("claude/opus"), "model missing");
        assert!(text.contains("42 tools"), "tool count missing");
        assert!(text.contains("connected"), "notice missing");
        assert!(text.contains("hello there"), "user line missing");
        assert!(text.contains("hi human"), "assistant text missing");
        assert!(text.contains("ready"), "status missing");
    }

    #[test]
    fn render_draws_a_framed_tool_cell() {
        let mut state = State::new("p", "m");
        state.start_tool("read_file", "{}");
        state.finish_tool_with("read_file", true, "line one\nline two", 320);
        let text = buffer_text(&state, 60, 14);
        assert!(text.contains("read_file"), "tool name: {text}");
        assert!(
            text.contains('╭') && text.contains('╯'),
            "frame glyphs: {text}"
        );
    }

    #[test]
    fn short_transcript_is_top_anchored() {
        let mut state = State::new("p", "m");
        state.push_user("first line");
        // Plenty of vertical room: the first line must appear (no scroll-off-top).
        let text = buffer_text(&state, 40, 20);
        assert!(text.contains("first line"));
    }
}
