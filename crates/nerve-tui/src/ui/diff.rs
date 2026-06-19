//! Unified-diff coloring, ported from the TS `diff.ts` (itself from oh-my-pi).
//!
//! Prefix coloring (added green / removed red / hunk cyan / context dim), dim
//! indentation glyphs (tabs → `→ `, spaces → `·`), and intra-line highlighting of
//! the differing middle on single-line replacements. Where TS used reverse-video
//! SGR (`\x1b[7m`) for the intra-line highlight, this uses ratatui
//! [`Modifier::REVERSED`](ratatui::style::Modifier::REVERSED).
//!
//! Output is one ratatui [`Line`] per diff line; each line's base color is folded
//! into its spans so the indent/intra-line styling layers on top.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::palette;
use super::width::sanitize;

/// Heuristic: does this text look like a unified diff? (Ports `isDiff`.)
#[must_use]
pub fn is_diff(text: &str) -> bool {
    if text.starts_with("@@ ") || text.contains("\n@@ ") {
        return true;
    }
    let has_minus = text.starts_with("--- ") || text.contains("\n--- ");
    let has_plus = text.starts_with("+++ ") || text.contains("\n+++ ");
    has_minus && has_plus
}

fn is_added(line: &str) -> bool {
    line.starts_with('+') && !line.starts_with("+++")
}

fn is_removed(line: &str) -> bool {
    line.starts_with('-') && !line.starts_with("---")
}

/// Render a unified-diff string to colored ratatui lines (ports `renderDiff`).
#[must_use]
pub fn render_diff(diff_text: &str) -> Vec<Line<'static>> {
    let sanitized = sanitize(diff_text);
    let lines: Vec<&str> = sanitized.split('\n').collect();
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        if line.starts_with("@@") {
            out.push(Line::from(Span::styled(line.to_string(), palette::cyan())));
            i += 1;
        } else if line.starts_with("+++") || line.starts_with("---") {
            out.push(Line::from(Span::styled(line.to_string(), palette::dim())));
            i += 1;
        } else if is_removed(line) {
            i = render_change_group(&lines, i, &mut out);
        } else if is_added(line) {
            out.push(added_line(&line[1..]));
            i += 1;
        } else if line.is_empty() {
            out.push(Line::from(""));
            i += 1;
        } else {
            out.push(Line::from(Span::styled(line.to_string(), palette::dim())));
            i += 1;
        }
    }
    out
}

/// Render a contiguous removed-then-added group starting at `start`. A 1:1
/// replacement gets intra-line highlighting; otherwise lines are emitted plainly
/// colored. Returns the index past the group.
fn render_change_group(lines: &[&str], start: usize, out: &mut Vec<Line<'static>>) -> usize {
    let mut i = start;
    let mut removed: Vec<&str> = Vec::new();
    while i < lines.len() && is_removed(lines[i]) {
        removed.push(&lines[i][1..]);
        i += 1;
    }
    let mut added: Vec<&str> = Vec::new();
    while i < lines.len() && is_added(lines[i]) {
        added.push(&lines[i][1..]);
        i += 1;
    }
    if removed.len() == 1 && added.len() == 1 {
        let (rm, ad) = intra_line(removed[0], added[0]);
        out.push(change_line('-', palette::red(), rm));
        out.push(change_line('+', palette::green(), ad));
    } else {
        for rm in &removed {
            out.push(removed_line(rm));
        }
        for ad in &added {
            out.push(added_line(ad));
        }
    }
    i
}

/// A removed/added line with no intra-line emphasis: a single emphasis-free
/// segment over the whole content.
fn removed_line(content: &str) -> Line<'static> {
    change_line('-', palette::red(), vec![Segment::plain(content)])
}

fn added_line(content: &str) -> Line<'static> {
    change_line('+', palette::green(), vec![Segment::plain(content)])
}

/// One emphasis-tagged slice of a diff line's content.
struct Segment {
    text: String,
    /// Reverse-video the differing middle.
    emphasized: bool,
}

impl Segment {
    fn plain(text: &str) -> Self {
        Self {
            text: text.to_string(),
            emphasized: false,
        }
    }

    fn emph(text: &str) -> Self {
        Self {
            text: text.to_string(),
            emphasized: true,
        }
    }
}

/// Build a `+`/`-` line: the prefix glyph + base color, with indent glyphs dimmed
/// and emphasized segments reversed. Mirrors `style.red("-" + visualizeIndent(…))`.
fn change_line(prefix: char, base: Style, segments: Vec<Segment>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![Span::styled(prefix.to_string(), base)];
    let mut first = true;
    for seg in segments {
        if first {
            // Visualize leading indentation only at the very start of the content.
            push_with_indent(&mut spans, &seg.text, base, seg.emphasized);
            first = false;
        } else {
            push_text(&mut spans, &seg.text, base, seg.emphasized);
        }
    }
    Line::from(spans)
}

/// Push content whose leading whitespace is rendered as dim indent glyphs.
fn push_with_indent(spans: &mut Vec<Span<'static>>, text: &str, base: Style, emph: bool) {
    let indent_len = text.chars().take_while(|c| *c == ' ' || *c == '\t').count();
    if indent_len > 0 {
        let dim = base.add_modifier(Modifier::DIM);
        let mut glyphs = String::new();
        for ch in text.chars().take(indent_len) {
            glyphs.push_str(if ch == '\t' { "→ " } else { "·" });
        }
        spans.push(Span::styled(glyphs, dim));
    }
    let rest: String = text.chars().skip(indent_len).collect();
    push_text(spans, &rest, base, emph);
}

fn push_text(spans: &mut Vec<Span<'static>>, text: &str, base: Style, emph: bool) {
    if text.is_empty() {
        return;
    }
    let style = if emph {
        base.add_modifier(Modifier::REVERSED)
    } else {
        base
    };
    spans.push(Span::styled(text.to_string(), style));
}

/// Split a single-line replacement into shared prefix / differing middle /
/// shared suffix, emphasizing the middle. Ports `intraLine`; operates on chars
/// (the TS used UTF-16 code units, close enough for terminal display).
fn intra_line(old: &str, new: &str) -> (Vec<Segment>, Vec<Segment>) {
    let o: Vec<char> = old.chars().collect();
    let n: Vec<char> = new.chars().collect();
    let min = o.len().min(n.len());
    let mut p = 0;
    while p < min && o[p] == n[p] {
        p += 1;
    }
    let mut s = 0;
    while s < min - p && o[o.len() - 1 - s] == n[n.len() - 1 - s] {
        s += 1;
    }
    let prefix: String = o[..p].iter().collect();
    let suffix: String = o[o.len() - s..].iter().collect();
    let old_mid: String = o[p..o.len() - s].iter().collect();
    let new_mid: String = n[p..n.len() - s].iter().collect();
    let removed = build_segments(&prefix, &old_mid, &suffix);
    let added = build_segments(&prefix, &new_mid, &suffix);
    (removed, added)
}

fn build_segments(prefix: &str, mid: &str, suffix: &str) -> Vec<Segment> {
    let mut segs = Vec::new();
    if !prefix.is_empty() {
        segs.push(Segment::plain(prefix));
    }
    if !mid.is_empty() {
        segs.push(Segment::emph(mid));
    }
    if !suffix.is_empty() {
        segs.push(Segment::plain(suffix));
    }
    if segs.is_empty() {
        segs.push(Segment::plain(""));
    }
    segs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn detects_unified_diffs() {
        assert!(is_diff("@@ -1 +1 @@\n-a\n+b"));
        assert!(is_diff("--- a/x\n+++ b/x\n@@ -1 +1 @@"));
        assert!(!is_diff("hello world"));
    }

    #[test]
    fn colors_added_removed_hunk_context() {
        let lines =
            render_diff("--- a/x\n+++ b/x\n@@ -1,1 +1,1 @@\n-old line\n+new line\n unchanged");
        let texts: Vec<String> = lines.iter().map(plain).collect();
        assert!(texts.iter().any(|t| t.contains("@@ -1,1 +1,1 @@")));
        assert!(texts.iter().any(|t| t.starts_with("-old line")));
        assert!(texts.iter().any(|t| t.starts_with("+new line")));
        assert!(texts.iter().any(|t| t == " unchanged"));
        // The hunk header is cyan.
        let hunk = lines.iter().find(|l| plain(l).starts_with("@@")).unwrap();
        assert_eq!(hunk.spans[0].style, palette::cyan());
    }

    #[test]
    fn intra_line_emphasizes_only_the_difference() {
        // "old line" vs "new line": shared " line" suffix, differing prefix word.
        let lines = render_diff("@@ x @@\n-old line\n+new line");
        let removed = lines.iter().find(|l| plain(l).starts_with("-")).unwrap();
        // The differing middle ("old"/"new") is reversed; the shared suffix isn't.
        assert!(
            removed
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::REVERSED))
        );
        assert!(
            removed.spans.iter().any(|s| s.content.contains("line")
                && !s.style.add_modifier.contains(Modifier::REVERSED))
        );
    }

    #[test]
    fn visualizes_leading_indent() {
        // `render_diff` sanitizes first (tabs → two spaces, matching TS), so the
        // indent visualizer sees spaces → dim `·` glyphs. A literal tab in the
        // source therefore renders as `··`, not `→ `.
        let lines = render_diff("@@ x @@\n+\tindented");
        let added = lines.iter().find(|l| plain(l).starts_with('+')).unwrap();
        let joined = plain(added);
        assert!(joined.contains('·'), "space glyph: {joined}");
        // The indent glyphs carry the dim modifier on top of the green base.
        assert!(
            added
                .spans
                .iter()
                .any(|s| s.content.contains('·') && s.style.add_modifier.contains(Modifier::DIM))
        );
    }
}
