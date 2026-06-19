//! Minimal markdown → styled lines, ported from the TS `markdown.ts`.
//!
//! Inline spans (`**bold**`, `*italic*`/`_italic_`, `` `code` ``), fenced code
//! blocks (highlighted via [`highlight`](super::highlight)), ATX headings,
//! bullet/numbered lists, and blockquotes. Inline runs are wrapped span-aware
//! (via [`wrap_runs`](super::width::wrap_runs)) so styling survives line wrapping.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::highlight::highlight;
use super::palette;
use super::width::{Run, truncate_to_width, wrap_runs};

/// Inline `code` is yellow (matches `markdown.ts`'s `CODE`).
fn code_style() -> Style {
    palette::yellow()
}

/// Parse inline markdown into styled runs. Unmatched markers stay literal.
/// Direct port of `parseInline`.
#[must_use]
pub fn parse_inline(text: &str) -> Vec<Run> {
    let chars: Vec<char> = text.chars().collect();
    let mut runs: Vec<Run> = Vec::new();
    let mut plain = String::new();
    let mut i = 0usize;
    let flush = |plain: &mut String, runs: &mut Vec<Run>| {
        if !plain.is_empty() {
            runs.push(Run::plain(std::mem::take(plain)));
        }
    };
    while i < chars.len() {
        if starts_with(&chars, i, "**")
            && let Some(end) = find(&chars, i + 2, "**")
        {
            flush(&mut plain, &mut runs);
            runs.push(Run::new(slice(&chars, i + 2, end), palette::bold()));
            i = end + 2;
            continue;
        }
        let ch = chars[i];
        if ch == '`'
            && let Some(end) = find_char(&chars, i + 1, '`')
        {
            flush(&mut plain, &mut runs);
            runs.push(Run::new(slice(&chars, i + 1, end), code_style()));
            i = end + 1;
            continue;
        }
        if (ch == '*' || ch == '_')
            && chars.get(i + 1).is_some_and(|n| !n.is_whitespace())
            && let Some(end) = find_char(&chars, i + 1, ch)
            && end > i + 1
        {
            flush(&mut plain, &mut runs);
            runs.push(Run::new(slice(&chars, i + 1, end), palette::italic()));
            i = end + 1;
            continue;
        }
        plain.push(ch);
        i += 1;
    }
    flush(&mut plain, &mut runs);
    runs
}

fn starts_with(chars: &[char], at: usize, pat: &str) -> bool {
    pat.chars()
        .enumerate()
        .all(|(k, c)| chars.get(at + k) == Some(&c))
}

fn find(chars: &[char], from: usize, pat: &str) -> Option<usize> {
    let pat: Vec<char> = pat.chars().collect();
    if pat.is_empty() || from > chars.len() {
        return None;
    }
    (from..=chars.len().saturating_sub(pat.len()))
        .find(|&start| chars[start..start + pat.len()] == pat[..])
}

fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|&k| chars[k] == target)
}

fn slice(chars: &[char], from: usize, to: usize) -> String {
    chars[from..to].iter().collect()
}

/// Render a list item with a styled (cyan) prefix and a hanging indent. Ports
/// `renderListItem`.
fn render_list_item(prefix: &str, body: &str, cols: usize) -> Vec<Line<'static>> {
    let prefix_width = prefix.chars().count();
    let wrapped = wrap_runs(
        &parse_inline(body),
        cols.saturating_sub(prefix_width).max(1),
    );
    let pad = " ".repeat(prefix_width);
    wrapped
        .into_iter()
        .enumerate()
        .map(|(idx, mut line)| {
            let lead = if idx == 0 {
                Span::styled(prefix.to_string(), palette::cyan())
            } else {
                Span::raw(pad.clone())
            };
            line.spans.insert(0, lead);
            line
        })
        .collect()
}

/// Emit a fenced code block: highlight each line, indent by two spaces, and
/// truncate to width (code is not wrapped — it scrolls). Ports `emitCodeFence`.
fn emit_code_fence(lang: Option<&str>, code_lines: &[String], cols: usize) -> Vec<Line<'static>> {
    let code = code_lines.join("\n");
    highlight(&code, lang)
        .into_iter()
        .map(|runs| indent_and_truncate(&runs, cols))
        .collect()
}

/// Prefix `  ` and truncate a styled code line to width, preserving per-run style.
fn indent_and_truncate(runs: &[Run], cols: usize) -> Line<'static> {
    let inner = cols.saturating_sub(2).max(1);
    let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
    let mut used = 0usize;
    let mut truncated = false;
    for run in runs {
        if truncated {
            break;
        }
        let remaining = inner.saturating_sub(used);
        let kept = truncate_to_width(&run.text, remaining + 1);
        // `truncate_to_width` appends '…' only when it actually dropped content.
        let ellipsized = kept.ends_with('…') && !run.text.ends_with('…');
        let text = if ellipsized { kept } else { run.text.clone() };
        used += super::width::width(&text);
        spans.push(Span::styled(text, run.style));
        if ellipsized || used >= inner {
            truncated = true;
        }
    }
    Line::from(spans)
}

/// Render markdown text to colored, wrapped terminal lines. Ports `renderMarkdown`.
#[must_use]
pub fn render_markdown(text: &str, cols: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut fence: Option<(Option<String>, Vec<String>)> = None;
    for raw in text.split('\n') {
        if let Some(lang) = fence_marker(raw) {
            match fence.take() {
                None => fence = Some((lang, Vec::new())),
                Some((flang, flines)) => {
                    out.extend(emit_code_fence(flang.as_deref(), &flines, cols));
                }
            }
            continue;
        }
        if let Some((_, lines)) = fence.as_mut() {
            lines.push(raw.to_string());
            continue;
        }
        render_line(raw, cols, &mut out);
    }
    if let Some((flang, flines)) = fence {
        out.extend(emit_code_fence(flang.as_deref(), &flines, cols));
    }
    out
}

/// Render one non-fence markdown line (heading / quote / list / paragraph).
fn render_line(raw: &str, cols: usize, out: &mut Vec<Line<'static>>) {
    if raw.trim().is_empty() {
        out.push(Line::from(""));
        return;
    }
    if let Some(body) = heading(raw) {
        let style = palette::cyan().add_modifier(ratatui::style::Modifier::BOLD);
        for line in wrap_runs(&parse_inline(body), cols) {
            out.push(restyle(line, style));
        }
        return;
    }
    if let Some(body) = blockquote(raw) {
        for line in wrap_runs(&parse_inline(body), cols.saturating_sub(2).max(1)) {
            let mut spans = vec![Span::styled("│ ".to_string(), palette::dim())];
            spans.extend(restyle(line, palette::dim()).spans);
            out.push(Line::from(spans));
        }
        return;
    }
    if let Some((indent, body)) = bullet(raw) {
        out.extend(render_list_item(&format!("{indent}• "), body, cols));
        return;
    }
    if let Some((indent, num, body)) = numbered(raw) {
        out.extend(render_list_item(&format!("{indent}{num}. "), body, cols));
        return;
    }
    out.extend(wrap_runs(&parse_inline(raw), cols));
}

/// Force `style` onto every span of a line (headings/quotes override inline).
fn restyle(line: Line<'static>, style: Style) -> Line<'static> {
    let spans = line
        .spans
        .into_iter()
        .map(|s| Span::styled(s.content, style.patch(s.style)))
        .collect::<Vec<_>>();
    Line::from(spans)
}

/// ```` ```lang ```` opener/closer detector → the (optional) language tag.
fn fence_marker(raw: &str) -> Option<Option<String>> {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("```") {
        return None;
    }
    let after = trimmed.trim_start_matches('`');
    let lang: String = after
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '+' || *c == '-')
        .collect();
    Some(if lang.is_empty() { None } else { Some(lang) })
}

fn heading(raw: &str) -> Option<&str> {
    let hashes = raw.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && raw.as_bytes().get(hashes) == Some(&b' ') {
        Some(raw[hashes + 1..].trim_start())
    } else {
        None
    }
}

fn blockquote(raw: &str) -> Option<&str> {
    let rest = raw.strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// `(\s*)[-*+]\s+(body)` → (leading indent, body).
fn bullet(raw: &str) -> Option<(&str, &str)> {
    let indent_len = raw.len() - raw.trim_start().len();
    let (indent, rest) = raw.split_at(indent_len);
    let marker = rest.chars().next()?;
    if !matches!(marker, '-' | '*' | '+') {
        return None;
    }
    let after = &rest[marker.len_utf8()..];
    if !after.starts_with(' ') && !after.starts_with('\t') {
        return None;
    }
    Some((indent, after.trim_start()))
}

/// `(\s*)(\d+)\.\s+(body)` → (leading indent, number, body).
fn numbered(raw: &str) -> Option<(&str, &str, &str)> {
    let indent_len = raw.len() - raw.trim_start().len();
    let (indent, rest) = raw.split_at(indent_len);
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || rest.as_bytes().get(digits) != Some(&b'.') {
        return None;
    }
    let after = &rest[digits + 1..];
    if !after.starts_with(' ') && !after.starts_with('\t') {
        return None;
    }
    Some((indent, &rest[..digits], after.trim_start()))
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
    fn parse_inline_splits_bold_code_runs() {
        let runs = parse_inline("a **b** `c`");
        assert!(
            runs.iter()
                .any(|r| r.text == "b" && r.style == palette::bold())
        );
        assert!(
            runs.iter()
                .any(|r| r.text == "c" && r.style == code_style())
        );
    }

    #[test]
    fn styles_headings_bullets_fences_inline() {
        let title = render_markdown("# Title", 40);
        assert!(plain(&title).iter().any(|l| l.contains("Title")));
        assert!(
            title[0]
                .spans
                .iter()
                .all(|s| s.style.fg == Some(ratatui::style::Color::Cyan))
        );

        let list = render_markdown("- one\n- two", 40);
        assert!(plain(&list).iter().any(|l| l.contains("• one")));

        let fence = render_markdown("```\ncode line\n```", 40);
        assert!(plain(&fence).iter().any(|l| l.contains("code line")));

        let inline = plain(&render_markdown("a **bold** b `code`", 40)).join(" ");
        assert!(inline.contains("bold"));
        assert!(inline.contains("code"));
    }

    #[test]
    fn highlights_fenced_code_by_language() {
        let lines = render_markdown("```ts\nconst x = 1\n```", 40);
        // The line is present and `const` is highlighted (not default style).
        let kw = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| s.content.as_ref() == "const" && s.style == palette::magenta());
        assert!(kw, "expected highlighted `const`");
    }

    #[test]
    fn numbered_and_quote() {
        let num = render_markdown("1. first", 40);
        assert!(plain(&num).iter().any(|l| l.starts_with("1. first")));
        let quote = render_markdown("> quoted", 40);
        assert!(plain(&quote).iter().any(|l| l.starts_with("│ quoted")));
    }
}
