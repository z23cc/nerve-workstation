//! Lightweight syntax highlighter for fenced code blocks.
//!
//! A direct port of the TS `highlight.ts` regex/state-machine lexer: keywords,
//! strings, numbers, and comments (incl. `/* */` block comments spanning lines)
//! for ts/js/rust/python/bash/go/json. Unknown languages are dimmed wholesale.
//! Invariant (as in TS): concatenating a line's run texts yields the source line
//! verbatim — only the styling differs.
//!
//! syntect would give richer, grammar-accurate highlighting; this hand lexer is
//! kept for now because it is deterministic and snapshot-friendly and matches the
//! TS output. Swapping in syntect is a future upgrade (noted for T-later).

use ratatui::style::Style;

use super::palette;
use super::width::Run;

/// One language's lexing rules (keyword set + comment markers).
struct LangSpec {
    keywords: &'static [&'static str],
    /// Line-comment markers (e.g. `//`, `#`).
    line: &'static [&'static str],
    /// Supports `/* … */` block comments.
    block: bool,
}

const TS_KW: &[&str] = &[
    "const",
    "let",
    "var",
    "function",
    "return",
    "if",
    "else",
    "for",
    "while",
    "class",
    "import",
    "export",
    "from",
    "default",
    "async",
    "await",
    "new",
    "type",
    "interface",
    "extends",
    "implements",
    "public",
    "private",
    "protected",
    "readonly",
    "this",
    "null",
    "undefined",
    "true",
    "false",
    "switch",
    "case",
    "break",
    "continue",
    "throw",
    "try",
    "catch",
    "finally",
    "typeof",
    "instanceof",
    "void",
    "enum",
    "as",
    "of",
    "in",
    "do",
    "yield",
    "static",
    "get",
    "set",
    "namespace",
    "declare",
    "keyof",
    "infer",
    "satisfies",
];

const RUST_KW: &[&str] = &[
    "fn", "let", "mut", "const", "static", "struct", "enum", "impl", "trait", "pub", "use", "mod",
    "match", "if", "else", "for", "while", "loop", "return", "self", "Self", "super", "crate",
    "where", "async", "await", "move", "ref", "dyn", "as", "in", "break", "continue", "type",
    "unsafe", "extern", "true", "false", "Some", "None", "Ok", "Err", "Box", "Vec", "String",
];

const PY_KW: &[&str] = &[
    "def", "class", "return", "if", "elif", "else", "for", "while", "import", "from", "as", "with",
    "try", "except", "finally", "raise", "lambda", "yield", "async", "await", "pass", "break",
    "continue", "global", "nonlocal", "True", "False", "None", "and", "or", "not", "in", "is",
    "del",
];

const BASH_KW: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "function", "return", "local", "export", "readonly", "declare", "in", "select",
];

const GO_KW: &[&str] = &[
    "func",
    "var",
    "const",
    "type",
    "struct",
    "interface",
    "package",
    "import",
    "return",
    "if",
    "else",
    "for",
    "range",
    "switch",
    "case",
    "default",
    "go",
    "defer",
    "chan",
    "map",
    "select",
    "break",
    "continue",
    "fallthrough",
    "nil",
    "true",
    "false",
];

const JSON_KW: &[&str] = &["true", "false", "null"];

/// Resolve a language tag (with aliases) to its [`LangSpec`].
fn spec_for(lang: Option<&str>) -> Option<LangSpec> {
    let key = lang?.to_ascii_lowercase();
    let canonical = match key.as_str() {
        "typescript" | "tsx" => "ts",
        "javascript" | "jsx" => "js",
        "rs" => "rust",
        "py" => "python",
        "sh" | "shell" | "zsh" => "bash",
        "golang" => "go",
        other => other,
    };
    let (keywords, line, block): (&[&str], &[&str], bool) = match canonical {
        "ts" | "js" => (TS_KW, &["//"], true),
        "rust" => (RUST_KW, &["//"], true),
        "python" => (PY_KW, &["#"], false),
        "bash" => (BASH_KW, &["#"], false),
        "go" => (GO_KW, &["//"], true),
        "json" => (JSON_KW, &[], false),
        _ => return None,
    };
    Some(LangSpec {
        keywords,
        line,
        block,
    })
}

fn paint_keyword() -> Style {
    palette::magenta()
}
fn paint_string() -> Style {
    palette::green()
}
fn paint_number() -> Style {
    palette::yellow()
}
fn paint_comment() -> Style {
    palette::gray()
}

/// Highlight `code` for `lang` into one [`Vec<Run>`] per source line. Unknown
/// languages dim every line. The outer `Vec` is lines; the inner is styled runs.
#[must_use]
pub fn highlight(code: &str, lang: Option<&str>) -> Vec<Vec<Run>> {
    let lines: Vec<&str> = code.split('\n').collect();
    let Some(spec) = spec_for(lang) else {
        return lines
            .into_iter()
            .map(|line| vec![Run::new(line.to_string(), palette::dim())])
            .collect();
    };
    let mut out = Vec::with_capacity(lines.len());
    let mut in_block = false;
    for line in lines {
        let (runs, next) = highlight_line(line, &spec, in_block);
        out.push(runs);
        in_block = next;
    }
    out
}

/// Find the end (exclusive) of a string literal starting at `start` (the quote).
/// Handles `\` escapes; runs to EOL if unterminated. Indices are byte offsets.
fn find_string_end(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

/// Highlight a single line; returns its styled runs and whether a block comment
/// remains open into the next line. Operates on ASCII byte offsets, falling back
/// to verbatim plain runs for the (rare) non-ASCII remainder, preserving the
/// "strip styling == source" invariant.
fn highlight_line(line: &str, spec: &LangSpec, mut in_block: bool) -> (Vec<Run>, bool) {
    let mut runs: Vec<Run> = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0usize;
    if in_block {
        if let Some(end) = line.find("*/") {
            push(&mut runs, &line[..end + 2], paint_comment());
            i = end + 2;
            in_block = false;
        } else {
            return (vec![Run::new(line.to_string(), paint_comment())], true);
        }
    }
    while i < bytes.len() {
        let rest = &line[i..];
        if spec.block && rest.starts_with("/*") {
            match rest.find("*/") {
                None => {
                    push(&mut runs, rest, paint_comment());
                    return (runs, true);
                }
                Some(end) => {
                    push(&mut runs, &rest[..end + 2], paint_comment());
                    i += end + 2;
                    continue;
                }
            }
        }
        if let Some(marker) = spec.line.iter().find(|m| rest.starts_with(**m)) {
            let _ = marker;
            push(&mut runs, rest, paint_comment());
            break;
        }
        let ch = bytes[i];
        if ch == b'"' || ch == b'\'' || ch == b'`' {
            let end = find_string_end(bytes, i, ch);
            push(&mut runs, &line[i..end], paint_string());
            i = end;
            continue;
        }
        if ch.is_ascii_digit() && !is_ident_byte(prev_byte(bytes, i)) {
            let end = number_end(bytes, i);
            push(&mut runs, &line[i..end], paint_number());
            i = end;
            continue;
        }
        if is_ident_start(ch) {
            let end = ident_end(bytes, i);
            let word = &line[i..end];
            let style = if spec.keywords.contains(&word) {
                paint_keyword()
            } else {
                Style::default()
            };
            push(&mut runs, word, style);
            i = end;
            continue;
        }
        // Single byte (or the start of a UTF-8 char): copy verbatim, unstyled.
        let chr_len = utf8_len(ch);
        push(&mut runs, &line[i..i + chr_len], Style::default());
        i += chr_len;
    }
    (runs, in_block)
}

/// Append text to `runs`, coalescing with the previous run when styles match.
fn push(runs: &mut Vec<Run>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut()
        && last.style == style
    {
        last.text.push_str(text);
        return;
    }
    runs.push(Run::new(text.to_string(), style));
}

fn prev_byte(bytes: &[u8], i: usize) -> u8 {
    if i == 0 { 0 } else { bytes[i - 1] }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$'
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn ident_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < bytes.len() && is_ident_byte(bytes[i]) {
        i += 1;
    }
    i
}

/// End of a number literal: `\d[\d_.eExXa-fA-F]*` (matches the TS regex).
fn number_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        let b = bytes[i];
        let ok = b.is_ascii_digit()
            || b == b'_'
            || b == b'.'
            || b == b'e'
            || b == b'E'
            || b == b'x'
            || b == b'X'
            || (b'a'..=b'f').contains(&b)
            || (b'A'..=b'F').contains(&b);
        if !ok {
            break;
        }
        i += 1;
    }
    i
}

/// Byte length of the UTF-8 char whose leading byte is `b`.
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(runs: &[Run]) -> String {
        runs.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn preserves_source_and_colors_known_languages() {
        let code = r#"const x = "hi"; // note"#;
        let lines = highlight(code, Some("ts"));
        assert_eq!(lines.len(), 1);
        assert_eq!(source(&lines[0]), code);
        // `const` is a keyword (magenta), the string is green, the comment gray.
        assert!(
            lines[0]
                .iter()
                .any(|r| r.style == palette::magenta() && r.text == "const")
        );
        assert!(
            lines[0]
                .iter()
                .any(|r| r.style == palette::green() && r.text.contains("hi"))
        );
        assert!(
            lines[0]
                .iter()
                .any(|r| r.style == palette::gray() && r.text.contains("note"))
        );
    }

    #[test]
    fn spans_block_comments_and_dims_unknown() {
        let lines = highlight("/* a\n b */ code", Some("rust"));
        assert_eq!(lines.len(), 2);
        assert_eq!(
            source(&lines[0]) + "\n" + &source(&lines[1]),
            "/* a\n b */ code"
        );
        // First line fully comment; second line opens with the comment close.
        assert!(lines[0].iter().all(|r| r.style == palette::gray()));

        let unknown = highlight("whatever", Some("klingon"));
        assert_eq!(source(&unknown[0]), "whatever");
        assert!(unknown[0].iter().all(|r| r.style == palette::dim()));
    }

    #[test]
    fn numbers_only_at_token_boundaries() {
        // `x1` is an identifier, not a number (digit after a letter); `42` is a
        // number. `x1` is unstyled, so it coalesces with surrounding plain text —
        // assert it lands in a default-styled run and is *not* colored as a number.
        let lines = highlight("let x1 = 42", Some("rust"));
        assert!(
            lines[0]
                .iter()
                .any(|r| r.text.contains("x1") && r.style == Style::default())
        );
        assert!(
            !lines[0]
                .iter()
                .any(|r| r.text.contains("x1") && r.style == palette::yellow())
        );
        assert!(
            lines[0]
                .iter()
                .any(|r| r.text == "42" && r.style == palette::yellow())
        );
    }
}
