//! Rich transcript rendering: turn conversation [`Block`](crate::app::state::Block)
//! values into width-wrapped ratatui [`Line`](ratatui::text::Line)s of styled
//! [`Span`](ratatui::text::Span)s.
//!
//! This layer emits ratatui `Style`/`Span` — wrapping, coloring, and truncation
//! operate on styled runs ([`width`]) rather than ANSI escape sequences, so which
//! spans get which color, where lines break, and which frame glyphs are used are
//! all decided here; deviations from the common defaults are noted at each call site.
//!
//! Submodules:
//! - [`width`] — sanitize, display width, styled wrapping,
//! - [`highlight`] — the regex/state-machine syntax highlighter,
//! - [`diff`] — unified-diff coloring (intra-line via `REVERSED`),
//! - [`markdown`] — markdown → styled lines,
//! - [`render`] — the `Block` → lines entry points,
//! - [`editor`] — the multiline input editor,
//! - [`commands`] — slash-command parsing + palette,
//! - [`models`] — per-model context-window + price table,
//! - [`theme`] — the cycled accent themes.

pub mod commands;
pub mod diff;
pub mod editor;
pub mod highlight;
pub mod markdown;
pub mod models;
pub mod render;
pub mod theme;
pub mod width;

use ratatui::style::{Color, Modifier, Style};

/// Fixed transcript palette, mirroring `ansi.ts::style`. Each maps a TS SGR
/// helper to the equivalent ratatui [`Style`]. Transcript colors are fixed (the
/// live-cycled accent theme is chrome only, handled in T3).
pub mod palette {
    use super::{Color, Modifier, Style};

    /// `style.dim` — faint text (reasoning, context, notices/info).
    #[must_use]
    pub fn dim() -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }

    /// `style.bold`.
    #[must_use]
    pub fn bold() -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    /// `style.italic`.
    #[must_use]
    pub fn italic() -> Style {
        Style::default().add_modifier(Modifier::ITALIC)
    }

    /// `style.cyan` — accent: user prompt, tool name, headings, hunk headers.
    #[must_use]
    pub fn cyan() -> Style {
        Style::default().fg(Color::Cyan)
    }

    /// `style.green` — success marker, added diff lines, code strings.
    #[must_use]
    pub fn green() -> Style {
        Style::default().fg(Color::Green)
    }

    /// `style.red` — error marker, removed diff lines, error output.
    #[must_use]
    pub fn red() -> Style {
        Style::default().fg(Color::Red)
    }

    /// `style.yellow` — running marker, code numbers, inline `code` spans.
    #[must_use]
    pub fn yellow() -> Style {
        Style::default().fg(Color::Yellow)
    }

    /// `style.magenta` — code keywords.
    #[must_use]
    pub fn magenta() -> Style {
        Style::default().fg(Color::Magenta)
    }

    /// `style.gray` — code comments (bright-black / ANSI 90).
    #[must_use]
    pub fn gray() -> Style {
        Style::default().fg(Color::DarkGray)
    }

    /// `style.invert` — intra-line diff highlight (reverse video).
    #[must_use]
    pub fn reversed() -> Style {
        Style::default().add_modifier(Modifier::REVERSED)
    }
}
