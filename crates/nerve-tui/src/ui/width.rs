//! Display-width, sanitization, and width-aware wrapping of styled spans.
//!
//! Ratatui carries style as `Style`/`Span` rather than inline SGR escapes, so the
//! TS helpers that did escape-string surgery (`ansi.ts`) become span operations
//! here: [`sanitize`] strips control sequences from untrusted text, [`width`]
//! measures display columns via the `unicode-width` crate, and [`wrap_runs`]
//! word-wraps a sequence of styled runs into `Vec<Line>` (the union of the TS
//! `wrapText` and `wrapRuns`). A *run* is a `(text, Style)` pair; wrapping splits
//! on whitespace, preserves each token's style, and hard-breaks overlong tokens.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// A styled text run: the wrapping unit. Mirrors the TS `Run` (text + render fn);
/// here the renderer is a ratatui [`Style`] applied to every token of `text`.
#[derive(Debug, Clone)]
pub struct Run {
    pub text: String,
    pub style: Style,
}

impl Run {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }

    /// A run with the default (unstyled) style — the TS `IDENTITY` render.
    pub fn plain(text: impl Into<String>) -> Self {
        Self::new(text, Style::default())
    }
}

/// Display width of a string, counting wide chars as 2 and zero-width as 0.
/// `unicode-width` already drops control chars; we treat them as the TS does
/// (zero), which is fine because callers [`sanitize`] untrusted text first.
#[must_use]
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Display width of a single char (wide = 2, zero-width/control = 0).
#[must_use]
fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Strip raw control sequences from untrusted content (model/tool output): tabs
/// become two spaces, CR is dropped, and every escape/control char except
/// newline is removed. Ports `ansi.ts::sanitize` so a hostile tool result can't
/// inject SGR/cursor moves into the styled buffer.
#[must_use]
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\t' => out.push_str("  "),
            '\r' => {}
            '\n' => out.push('\n'),
            // ESC: drop the whole CSI/escape sequence (consume until a final byte).
            '\u{1b}' => consume_escape(&mut chars),
            // Other C0/C1 control chars (keep nothing).
            c if is_control(c) => {}
            c => out.push(c),
        }
    }
    out
}

/// Consume an escape sequence following an already-read ESC. Handles CSI
/// (`ESC [ … final`), and short `ESC <byte>` forms — matching the two control
/// regexes in `ansi.ts::sanitize`.
fn consume_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.peek().copied() {
        Some('[') => {
            chars.next();
            // Parameter/intermediate bytes, then a final byte in @-~.
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        }
        Some(c) if ('\u{40}'..='\u{5f}').contains(&c) || c == '\\' || c == '_' => {
            chars.next();
        }
        _ => {}
    }
}

fn is_control(ch: char) -> bool {
    let c = ch as u32;
    c <= 0x08 || c == 0x0b || c == 0x0c || (0x0e..=0x1f).contains(&c)
}

/// Truncate a plain string to `cols` display columns, appending `…` when content
/// is dropped (the styled analogue of `ansi.ts::truncateToWidth`). Returns the
/// kept text plus whether an ellipsis was added, so callers can style the marker.
#[must_use]
pub fn truncate_to_width(text: &str, cols: usize) -> String {
    if cols == 0 {
        return String::new();
    }
    if width(text) <= cols {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = char_width(ch);
        if used + w > cols.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Word-wrap a single styled run to `cols` columns. Convenience over
/// [`wrap_runs`] for the common single-style case (the TS `wrapText` + a color).
#[must_use]
pub fn wrap_styled(text: &str, cols: usize, style: Style) -> Vec<Line<'static>> {
    wrap_runs(&[Run::new(text, style)], cols)
}

/// Wrap styled runs to `cols` columns, returning ratatui [`Line`]s of [`Span`]s.
///
/// This is the span-aware union of `wrapText` and `wrapRuns`:
/// - honors existing `\n` (each becomes a line break, blank lines preserved),
/// - splits on whitespace, dropping the leading space of a continued line,
/// - keeps a token's style across the break,
/// - hard-breaks a token wider than `cols`.
///
/// Adjacent same-style chunks are coalesced into one [`Span`] per line so the
/// buffer/snapshot output stays compact.
#[must_use]
pub fn wrap_runs(runs: &[Run], cols: usize) -> Vec<Line<'static>> {
    if cols == 0 {
        // Degenerate width: emit a single line with every run as-is.
        let spans = runs
            .iter()
            .filter(|r| !r.text.is_empty())
            .map(|r| Span::styled(r.text.clone(), r.style))
            .collect::<Vec<_>>();
        return vec![Line::from(spans)];
    }
    let mut builder = LineBuilder::new(cols);
    for run in runs {
        builder.push_run(run);
    }
    builder.finish()
}

/// Accumulates styled tokens into wrapped lines. Token-by-token so a run that
/// spans a line break keeps its style on both sides.
struct LineBuilder {
    cols: usize,
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    line_width: usize,
    /// Was the source split on `\n` (so we emit blank lines verbatim)?
    saw_any: bool,
}

impl LineBuilder {
    fn new(cols: usize) -> Self {
        Self {
            cols,
            lines: Vec::new(),
            current: Vec::new(),
            line_width: 0,
            saw_any: false,
        }
    }

    fn break_line(&mut self) {
        self.lines
            .push(Line::from(std::mem::take(&mut self.current)));
        self.line_width = 0;
    }

    fn push_span(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        // Coalesce with the previous span when styles match.
        if let Some(last) = self.current.last_mut()
            && last.style == style
        {
            last.content.to_mut().push_str(text);
            return;
        }
        self.current.push(Span::styled(text.to_string(), style));
    }

    fn push_run(&mut self, run: &Run) {
        self.saw_any = true;
        // Split on newlines first; each newline forces a line break.
        let mut segments = run.text.split('\n').peekable();
        while let Some(segment) = segments.next() {
            self.push_segment(segment, run.style);
            if segments.peek().is_some() {
                self.break_line();
            }
        }
    }

    /// Wrap one newline-free segment of a run.
    fn push_segment(&mut self, segment: &str, style: Style) {
        for token in split_whitespace_keep(segment) {
            let token_width = width(token);
            let is_space = token.chars().all(char::is_whitespace);
            if self.line_width == 0 && is_space {
                continue; // no leading space on a fresh line
            }
            if self.line_width + token_width <= self.cols {
                self.push_span(token, style);
                self.line_width += token_width;
                continue;
            }
            if self.line_width > 0 {
                self.break_line();
                if is_space {
                    continue;
                }
            }
            if token_width <= self.cols {
                self.push_span(token, style);
                self.line_width = token_width;
            } else {
                self.hard_break(token, style);
            }
        }
    }

    /// Hard-break a token wider than `cols`, char by char.
    fn hard_break(&mut self, token: &str, style: Style) {
        for ch in token.chars() {
            let cw = char_width(ch);
            if self.line_width + cw > self.cols {
                self.break_line();
            }
            let mut buf = [0u8; 4];
            self.push_span(ch.encode_utf8(&mut buf), style);
            self.line_width += cw;
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        // Mirror TS: push the trailing line if non-empty or nothing emitted yet.
        if !self.current.is_empty() || self.lines.is_empty() || self.saw_any {
            self.lines
                .push(Line::from(std::mem::take(&mut self.current)));
        }
        self.lines
    }
}

/// Split keeping whitespace tokens, like JS `split(/(\s+)/)` minus empties.
fn split_whitespace_keep(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_space: Option<bool> = None;
    for (idx, ch) in text.char_indices() {
        let space = ch.is_whitespace();
        match in_space {
            Some(prev) if prev == space => {}
            Some(_) => {
                out.push(&text[start..idx]);
                start = idx;
            }
            None => {}
        }
        in_space = Some(space);
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn width_counts_wide_chars_as_two() {
        assert_eq!(width("hello"), 5);
        assert_eq!(width("你好"), 4);
        assert_eq!(width("a你b"), 4);
    }

    #[test]
    fn wrap_hard_wraps_and_honors_newlines() {
        let out = wrap_styled("one two three", 7, Style::default());
        assert_eq!(plain(&out), vec!["one two", "three"]);
        let nl = wrap_styled("a\nb", 10, Style::default());
        assert_eq!(plain(&nl), vec!["a", "b"]);
        let long = wrap_styled("abcdefghij", 4, Style::default());
        assert!(long.iter().all(|l| width(&l.to_string()) <= 4));
        assert_eq!(plain(&long).join(""), "abcdefghij");
    }

    #[test]
    fn wrap_wide_line_respects_cell_width() {
        // Each CJK char is 2 cells; width 4 fits two per line.
        let out = wrap_styled("你好世界", 4, Style::default());
        assert!(out.iter().all(|l| width(&l.to_string()) <= 4));
        assert_eq!(plain(&out).join(""), "你好世界");
    }

    #[test]
    fn truncate_adds_ellipsis_within_width() {
        let out = truncate_to_width("abcdefghij", 5);
        assert!(width(&out) <= 5);
        assert!(out.ends_with('…'));
        assert_eq!(truncate_to_width("abc", 10), "abc");
    }

    #[test]
    fn sanitize_strips_control_and_escapes_expands_tabs() {
        assert_eq!(sanitize("a\tb"), "a  b");
        assert_eq!(sanitize("a\r\nb"), "a\nb");
        assert_eq!(sanitize("a\x1b[31mred\x1b[0mb"), "aredb");
        assert_eq!(sanitize("x\x07y"), "xy");
    }

    #[test]
    fn wrap_runs_keeps_style_across_break() {
        let bold = Style::default().add_modifier(ratatui::style::Modifier::BOLD);
        let runs = vec![Run::new("aaaa bbbb", bold)];
        let out = wrap_runs(&runs, 4);
        assert_eq!(plain(&out), vec!["aaaa", "bbbb"]);
        assert!(out.iter().all(|l| l.spans.iter().all(|s| s.style == bold)));
    }
}
