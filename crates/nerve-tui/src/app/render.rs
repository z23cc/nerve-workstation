//! Frame composition: a pure [`State`] → styled lines + cursor position, plus the
//! production [`render`] that paints them into a ratatui [`Frame`].
//!
//! Ports the layout of `packages/tui/src/ui/app.ts` (`renderFrame`, `headerLine`,
//! `statusLine`, `transcriptViewport`, `inputBlock`, `paletteLines`). Where the TS
//! emitted ANSI strings, this builds ratatui [`Line`]s of styled [`Span`]s; the
//! row math (header / transcript / palette / status / input, top-anchored, with a
//! flush-right token meter) matches the TS, including the multi-row input window
//! and the bare-slash command palette. The approval modal body (T4) is stubbed:
//! T3 reserves the row and the cursor-suppression branch.

use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::state::{Mode, State};
use crate::ui::commands::{CommandSpec, approval_mode_label, match_commands};
use crate::ui::editor::EditorLayout;
use crate::ui::models::model_info;
use crate::ui::palette;
use crate::ui::render::{self, RenderOptions, SPINNER, format_duration};
use crate::ui::width::{truncate_to_width, width as disp_width};

/// Max rows the input editor expands to before it scrolls internally. Ports
/// `MAX_INPUT_ROWS`.
const MAX_INPUT_ROWS: usize = 6;
/// Max palette rows shown at once. Ports the TS `Math.min(palette.length, 6)`.
const MAX_PALETTE_ROWS: usize = 6;

/// A composed frame: every row as a styled [`Line`], plus the input cursor's
/// (col, row) when the editor is focused (`None` in the approval modal). The
/// pure core both [`render`] and the tests consume.
#[derive(Debug)]
pub struct Composed {
    pub lines: Vec<Line<'static>>,
    pub cursor: Option<(u16, u16)>,
}

/// Paint the whole frame for the current terminal size, positioning the cursor.
pub fn render(frame: &mut Frame, state: &State) {
    let area = frame.area();
    let composed = compose(state, area.width as usize, area.height as usize);
    frame.render_widget(Paragraph::new(composed.lines), area);
    if let Some((col, row)) = composed.cursor {
        frame.set_cursor_position((area.x + col, area.y + row));
    }
}

/// Compose the frame as styled lines + cursor. Pure; the heart of the renderer.
/// Ports `renderFrame`: header, transcript viewport, optional palette, status,
/// then the input rows; clamped to `height`.
#[must_use]
pub fn compose(state: &State, width: usize, height: usize) -> Composed {
    let palette = if state.mode == Mode::Input {
        match_commands(state.editor.value())
    } else {
        Vec::new()
    };
    let palette_height = palette.len().min(MAX_PALETTE_ROWS);
    let input = input_block(state, width);
    let input_height = input.lines.len();
    let rows = height
        .saturating_sub(2)
        .saturating_sub(palette_height)
        .saturating_sub(input_height)
        .max(1);

    let mut lines = vec![header_line(state, width)];
    lines.extend(transcript_viewport(state, width, rows));
    if palette_height > 0 {
        let selected = (state.palette_index % palette.len()).min(palette_height - 1);
        lines.extend(palette_lines(
            state,
            &palette[..palette_height],
            selected,
            width,
        ));
    }
    lines.push(status_line(state, width));
    lines.extend(input.lines);
    lines.truncate(height);

    let cursor = input.cursor_row.map(|crow| {
        let row = (height.saturating_sub(input_height) + crow) as u16;
        let col = (2 + input.cursor_col) as u16;
        (col, row)
    });
    Composed { lines, cursor }
}

/// The header: `⬡ Nerve  <provider>/<model>  · N tools  · mode: <label>`, accent
/// logo over a reversed bar. Ports `headerLine`.
fn header_line(state: &State, width: usize) -> Line<'static> {
    let accent = Style::default().fg(state.accent());
    let mode = approval_mode_label(state.approval_mode);
    let dim = palette::dim();
    let spans = vec![
        Span::styled(" ⬡ Nerve  ", accent),
        Span::styled(format!("{}/{}", state.provider, state.model), dim),
        Span::styled(format!("  · {} tools", state.tools), dim),
        Span::styled(format!("  · mode: {mode}"), dim),
    ];
    reversed_bar(Line::from(spans), width)
}

/// Format a token count compactly (`12.3k`). Ports `formatTokens`.
fn format_tokens(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{n}")
    }
}

/// The status line: a left body (hint / working spinner+elapsed / ready) and a
/// flush-right token meter (`↑in ↓out · ctx% · $cost`). Ports `statusLine`.
fn status_line(state: &State, width: usize) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    spans.extend(status_body(state));
    let meter = token_meter(state);
    let left_width: usize = spans.iter().map(|s| disp_width(s.content.as_ref())).sum();
    let meter_width: usize = meter.iter().map(|s| disp_width(s.content.as_ref())).sum();
    if !meter.is_empty() && left_width + meter_width < width {
        spans.push(Span::raw(" ".repeat(width - left_width - meter_width)));
        spans.extend(meter);
    }
    reversed_bar(Line::from(spans), width)
}

/// The left body of the status line.
fn status_body(state: &State) -> Vec<Span<'static>> {
    if !state.hint.is_empty() {
        return vec![Span::styled(state.hint.clone(), palette::yellow())];
    }
    if state.running {
        return vec![
            Span::raw(format!(
                "{} working… ",
                SPINNER[state.spinner % SPINNER.len()]
            )),
            Span::raw(format_duration(state.elapsed_ms)),
            Span::raw("  "),
            Span::styled("Ctrl-C interrupt", palette::dim()),
        ];
    }
    vec![
        Span::styled("●", palette::green()),
        Span::raw(" ready  "),
        Span::styled(
            "/help · ↑↓ history · ⌥↵ newline · Ctrl-C quit",
            palette::dim(),
        ),
    ]
}

/// The flush-right token/context/cost meter, or empty when no tokens seen yet.
/// Ports the `tokens` composition in `statusLine`.
fn token_meter(state: &State) -> Vec<Span<'static>> {
    if state.tokens_in == 0 && state.tokens_out == 0 {
        return Vec::new();
    }
    let mut text = format!(
        "↑{} ↓{}",
        format_tokens(state.tokens_in),
        format_tokens(state.tokens_out)
    );
    if let Some(info) = model_info(&state.model)
        && state.last_context_tokens > 0
    {
        let pct = ((state.last_context_tokens as f64 / info.context_window as f64) * 100.0).round();
        text.push_str(&format!(" · {pct}%"));
    }
    if state.cost_usd >= 0.0005 {
        let prec = if state.cost_usd < 1.0 { 3 } else { 2 };
        text.push_str(&format!(" · ${:.*}", prec, state.cost_usd));
    }
    text.push(' ');
    vec![Span::styled(text, palette::dim())]
}

/// The transcript viewport: render every block, window to `rows` honoring the
/// scroll offset, and top-anchor a short transcript by padding below. Ports
/// `transcriptViewport`.
fn transcript_viewport(state: &State, width: usize, rows: usize) -> Vec<Line<'static>> {
    let opts = RenderOptions {
        spinner: state.spinner,
    };
    let mut blocks = state.blocks.clone();
    if state.expand_tools {
        for block in &mut blocks {
            if let super::state::Block::Tool(cell) = block {
                cell.collapsed = false;
            }
        }
    }
    let all = render::blocks_to_lines(&blocks, width, opts);
    let max_scroll = all.len().saturating_sub(rows);
    let scroll = state.scroll.min(max_scroll);
    let end = all.len() - scroll;
    let start = end.saturating_sub(rows);
    let mut view: Vec<Line<'static>> = all[start..end].to_vec();
    while view.len() < rows {
        view.push(Line::from(""));
    }
    view
}

/// One slash-command palette row, the selected one inverted in the accent.
/// Ports `paletteLines`.
fn palette_lines(
    state: &State,
    specs: &[CommandSpec],
    selected: usize,
    width: usize,
) -> Vec<Line<'static>> {
    specs
        .iter()
        .enumerate()
        .map(|(idx, spec)| {
            let spans = vec![
                Span::raw(format!(" /{}  ", spec.name)),
                Span::styled(spec.hint.to_string(), palette::dim()),
            ];
            let line = pad_to(truncate_line(Line::from(spans), width), width);
            if idx == selected {
                let accent = Style::default()
                    .fg(state.accent())
                    .add_modifier(Modifier::REVERSED);
                restyle(line, accent)
            } else {
                line
            }
        })
        .collect()
}

/// The rendered input rows + the cursor (row, col) within them. In the approval
/// modal the cursor is suppressed (T4 renders the body). Ports `inputBlock`.
struct InputBlock {
    lines: Vec<Line<'static>>,
    /// Cursor row within `lines`, or `None` to hide the cursor (approval modal).
    cursor_row: Option<usize>,
    cursor_col: usize,
}

fn input_block(state: &State, width: usize) -> InputBlock {
    if state.mode == Mode::Approval {
        // T4 fills the modal; T3 reserves a single placeholder row and hides the
        // cursor so the layout/row math is already correct.
        let body = state
            .approval
            .as_ref()
            .map(|a| format!(" approval pending: {} ", a.tool))
            .unwrap_or_default();
        return InputBlock {
            lines: vec![truncate_line(
                Line::from(Span::styled(body, palette::yellow())),
                width,
            )],
            cursor_row: None,
            cursor_col: 0,
        };
    }
    let EditorLayout {
        rows,
        cursor_row,
        cursor_col,
    } = state.editor.layout();
    let avail = width.saturating_sub(2).max(1);
    let visible = rows.len().min(MAX_INPUT_ROWS);
    let top = if rows.len() > MAX_INPUT_ROWS {
        cursor_row
            .saturating_sub(MAX_INPUT_ROWS - 1)
            .min(rows.len() - MAX_INPUT_ROWS)
    } else {
        0
    };
    let accent = Style::default().fg(state.accent());
    let mut lines = Vec::with_capacity(visible);
    for i in 0..visible {
        let global_row = top + i;
        let text = horizontal_window(rows.get(global_row).map_or("", String::as_str), avail);
        let marker = if global_row == 0 {
            Span::styled("❯ ", accent)
        } else {
            Span::raw("  ")
        };
        lines.push(Line::from(vec![marker, Span::raw(text)]));
    }
    InputBlock {
        lines,
        cursor_row: Some(cursor_row - top),
        cursor_col: cursor_col.min(avail.saturating_sub(1)),
    }
}

/// Keep the tail of an over-long input row visible (horizontal scroll). Ports the
/// TS `while (stringWidth(text) > avail) text = text.slice(1)`.
fn horizontal_window(text: &str, avail: usize) -> String {
    if disp_width(text) <= avail {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while disp_width(&chars.iter().collect::<String>()) > avail && !chars.is_empty() {
        chars.remove(0);
    }
    chars.into_iter().collect()
}

/// Wrap a line in a full-width reversed bar (header/status chrome): truncate to
/// width, then right-pad with reversed blanks. Ports `style.invert(padTo(...))`.
fn reversed_bar(line: Line<'static>, width_cols: usize) -> Line<'static> {
    let padded = pad_to(truncate_line(line, width_cols), width_cols);
    restyle(padded, Style::default().add_modifier(Modifier::REVERSED))
}

/// Apply a base style (merged under each span's own style) to a whole line. Used
/// to drop the reversed/accent chrome over already-colored spans.
fn restyle(line: Line<'static>, base: Style) -> Line<'static> {
    let spans = line
        .spans
        .into_iter()
        .map(|s| {
            let style = base.patch(s.style);
            Span::styled(s.content, style)
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

/// Right-pad a line with spaces to exactly `cols` columns (no truncation).
fn pad_to(line: Line<'static>, cols: usize) -> Line<'static> {
    let w: usize = line
        .spans
        .iter()
        .map(|s| disp_width(s.content.as_ref()))
        .sum();
    let mut spans = line.spans;
    if w < cols {
        spans.push(Span::raw(" ".repeat(cols - w)));
    }
    Line::from(spans)
}

/// Truncate a styled line to `cols` columns, span-by-span, appending a `…`.
fn truncate_line(line: Line<'static>, cols: usize) -> Line<'static> {
    let total: usize = line
        .spans
        .iter()
        .map(|s| disp_width(s.content.as_ref()))
        .sum();
    if cols == 0 {
        return Line::from("");
    }
    if total <= cols {
        return line;
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in line.spans {
        let remaining = cols.saturating_sub(1).saturating_sub(used);
        if remaining == 0 {
            break;
        }
        let kept = truncate_to_width(span.content.as_ref(), remaining + 1);
        let ellipsized = kept.ends_with('…') && !span.content.ends_with('…');
        let text = if ellipsized {
            kept.trim_end_matches('…').to_string()
        } else {
            kept
        };
        used += disp_width(&text);
        spans.push(Span::styled(text, span.style));
        if used >= cols.saturating_sub(1) {
            break;
        }
    }
    spans.push(Span::styled("…".to_string(), palette::dim()));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::ApprovalState;
    use nerve_runtime::{ApprovalMode, RiskTier};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn plain(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn sample() -> State {
        let mut state = State::new("xai", "grok-4-fast");
        state.tools = 32;
        state.push_user("hello");
        state.append_assistant("hi there");
        state.end_stream();
        state.editor.insert("type here");
        state
    }

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
    fn lays_out_header_transcript_status_input_and_fills_screen() {
        let composed = compose(&sample(), 40, 12);
        assert_eq!(composed.lines.len(), 12);
        assert!(plain(&composed.lines[0]).contains("Nerve"));
        assert!(plain(&composed.lines[0]).contains("xai/grok-4-fast"));
        assert!(
            plain(&composed.lines[10]).contains("ready")
                || plain(&composed.lines[10]).contains("working")
        );
        assert!(plain(&composed.lines[11]).contains("❯ type here"));
        assert!(composed.lines.iter().any(|l| plain(l).contains("hello")));
        assert!(composed.lines.iter().any(|l| plain(l).contains("hi there")));
        let (_, row) = composed.cursor.expect("cursor");
        assert_eq!(row, 11);
    }

    #[test]
    fn header_shows_approval_mode_label() {
        let mut yolo = sample();
        yolo.approval_mode = ApprovalMode::Yolo;
        assert!(plain(&compose(&yolo, 80, 12).lines[0]).contains("mode: yolo"));
        let mut ask = sample();
        ask.approval_mode = ApprovalMode::AlwaysAsk;
        assert!(plain(&compose(&ask, 80, 12).lines[0]).contains("mode: always-ask"));
    }

    #[test]
    fn status_shows_context_pct_and_cost_when_model_known() {
        let mut state = sample();
        state.model = "grok-4-fast".into();
        state.tokens_in = 128_000;
        state.tokens_out = 500;
        state.last_context_tokens = 128_000;
        state.cost_usd = 0.05;
        let status = plain(&compose(&state, 100, 12).lines[10]);
        assert!(status.contains("50%"), "{status}");
        assert!(status.contains("$0.05"), "{status}");
    }

    #[test]
    fn status_shows_token_usage_flush_right() {
        let mut state = sample();
        state.tokens_in = 12345;
        state.tokens_out = 678;
        let status = plain(&compose(&state, 100, 12).lines[10]);
        assert!(status.contains("↑12.3k"), "{status}");
        assert!(status.contains("↓678"), "{status}");
    }

    #[test]
    fn input_grows_to_multiple_rows() {
        let mut state = sample();
        state.editor.set_value("line1\nline2");
        let composed = compose(&state, 40, 12);
        assert_eq!(composed.lines.len(), 12);
        assert!(plain(&composed.lines[10]).contains("line1"));
        assert!(plain(&composed.lines[11]).contains("line2"));
        let (_, row) = composed.cursor.expect("cursor");
        assert_eq!(row, 11);
    }

    #[test]
    fn shows_palette_for_bare_slash_prefix() {
        let mut state = sample();
        state.editor.set_value("/m");
        let composed = compose(&state, 50, 14);
        assert!(composed.lines.iter().any(|l| plain(l).contains("/model")));
        assert!(composed.lines.iter().any(|l| plain(l).contains("/models")));
    }

    #[test]
    fn approval_mode_hides_the_cursor() {
        let mut state = sample();
        state.mode = Mode::Approval;
        state.approval = Some(ApprovalState {
            tool: "edit".into(),
            args: "{}".into(),
            request_id: "r".into(),
            session_id: "s".into(),
            tier: RiskTier::Edit,
            preview: String::new(),
        });
        let composed = compose(&state, 80, 12);
        assert!(composed.cursor.is_none());
    }

    #[test]
    fn renders_into_test_backend_with_header_and_prompt() {
        let text = buffer_text(&sample(), 60, 12);
        assert!(text.contains("Nerve"), "{text}");
        assert!(text.contains("type here"), "{text}");
    }
}
