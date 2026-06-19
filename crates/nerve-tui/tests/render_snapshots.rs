//! Insta snapshots of the rich renderer's styled output. Each snapshot renders a
//! representative block to `Vec<Line>` and serializes it as `text␟style` per line
//! so both the glyphs *and* the colors/modifiers are pinned (deterministic, no
//! wall-clock — tool durations are passed explicitly). These guard the pixel-level
//! port of the TS transcript/markdown/highlight/diff against drift.

use nerve_tui::app::state::{Block, Tone, ToolCall, ToolStatus};
use nerve_tui::ui::render::{RenderOptions, render_block};
use ratatui::style::{Color, Modifier};
use ratatui::text::Line;

/// Render a block and serialize each line as `[style]text` segments, one line per
/// row, so the snapshot captures glyphs + styling in a readable form.
fn styled(block: &Block, cols: usize) -> String {
    let lines = render_block(block, cols, RenderOptions::default());
    lines.iter().map(fmt_line).collect::<Vec<_>>().join("\n")
}

fn fmt_line(line: &Line<'static>) -> String {
    if line.spans.is_empty() {
        return String::new();
    }
    line.spans
        .iter()
        .map(|s| {
            let tag = style_tag(&s.style);
            if tag.is_empty() {
                s.content.to_string()
            } else {
                format!("«{tag}»{}", s.content)
            }
        })
        .collect()
}

/// Compact, stable tag for a ratatui `Style` (fg color + key modifiers).
fn style_tag(style: &ratatui::style::Style) -> String {
    let mut parts = Vec::new();
    if let Some(fg) = style.fg {
        parts.push(color_name(fg));
    }
    let m = style.add_modifier;
    if m.contains(Modifier::BOLD) {
        parts.push("bold");
    }
    if m.contains(Modifier::DIM) {
        parts.push("dim");
    }
    if m.contains(Modifier::ITALIC) {
        parts.push("italic");
    }
    if m.contains(Modifier::REVERSED) {
        parts.push("rev");
    }
    parts.join("+")
}

fn color_name(c: Color) -> &'static str {
    match c {
        Color::Cyan => "cyan",
        Color::Green => "green",
        Color::Red => "red",
        Color::Yellow => "yellow",
        Color::Magenta => "magenta",
        Color::DarkGray => "gray",
        _ => "fg",
    }
}

#[test]
fn snapshot_tool_frame_with_text_output() {
    let block = Block::Tool(ToolCall {
        tool: "read_file".into(),
        args: r#"{"path":"src/main.rs"}"#.into(),
        status: ToolStatus::Ok,
        output: Some("fn main() {\n    println!(\"hi\");\n}".into()),
        duration_ms: Some(1500),
        collapsed: false,
        started_at: None,
    });
    insta::assert_snapshot!(styled(&block, 40));
}

#[test]
fn snapshot_markdown_with_code_fence() {
    let md = "# Heading\n\nSome **bold** and `code`.\n\n- one\n- two\n\n```rust\nlet x = 42; // note\n```";
    let block = Block::Assistant(md.into());
    insta::assert_snapshot!(styled(&block, 40));
}

#[test]
fn snapshot_diff_tool_cell() {
    let output = serde_json::json!({
        "diff": "--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n context\n-old line here\n+new line here"
    })
    .to_string();
    let block = Block::Tool(ToolCall {
        tool: "edit".into(),
        args: r#"{"path":"x"}"#.into(),
        status: ToolStatus::Ok,
        output: Some(output),
        duration_ms: Some(42),
        collapsed: false,
        started_at: None,
    });
    insta::assert_snapshot!(styled(&block, 50));
}

#[test]
fn snapshot_reasoning_and_notice() {
    let reasoning = styled(&Block::Reasoning("considering the options".into()), 40);
    let warn = styled(
        &Block::Notice {
            tone: Tone::Warn,
            text: "interrupted: user".into(),
        },
        40,
    );
    insta::assert_snapshot!(format!("{reasoning}\n--\n{warn}"));
}
