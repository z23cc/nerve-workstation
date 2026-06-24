use super::*;

pub(super) fn import_references_for_language(
    language: Language,
    source: &str,
) -> Vec<CodeReference> {
    match language {
        Language::Rust => rust_import_references(source),
        Language::Python => python_import_references(source),
        Language::JavaScript | Language::TypeScript | Language::Tsx => {
            javascript_import_references(source)
        }
        _ => Vec::new(),
    }
}

fn rust_import_references(source: &str) -> Vec<CodeReference> {
    let mut out = Vec::new();
    let mut in_block_comment = false;
    for (line_index, raw_line) in source.lines().enumerate() {
        let cleaned = strip_c_like_block_comments(raw_line, &mut in_block_comment);
        let line = strip_line_comment(&cleaned, "//");
        let Some(use_body) = rust_use_body(line) else {
            continue;
        };
        if let Some((prefix, group)) = split_braced_import(use_body) {
            push_group_imports(&mut out, raw_line, line_index + 1, prefix, group, "::");
        } else {
            push_simple_import(&mut out, raw_line, line_index + 1, use_body, "::");
        }
    }
    out
}

fn python_import_references(source: &str) -> Vec<CodeReference> {
    let mut out = Vec::new();
    let mut triple_quote: Option<&'static str> = None;
    for (line_index, raw_line) in source.lines().enumerate() {
        // When no triple-quoted string is open, a `#` outside a string starts a
        // comment, so strip it before scanning for triple quotes — otherwise a
        // `'''`/`"""` inside that comment would wrongly open a docstring and
        // swallow the following `from ... import ...` lines. While a string IS
        // active, feed the whole line so a real `#` inside the docstring is not
        // mistaken for a comment.
        let scan = if triple_quote.is_none() {
            strip_line_comment(raw_line, "#")
        } else {
            raw_line
        };
        let cleaned = strip_python_triple_quotes(scan, &mut triple_quote);
        let line = strip_line_comment(&cleaned, "#").trim();
        let Some(rest) = line.strip_prefix("from ") else {
            continue;
        };
        let Some((module, names)) = rest.split_once(" import ") else {
            continue;
        };
        let search_start = raw_line
            .find(" import ")
            .map_or(0, |idx| idx + " import ".len());
        for item in names.split(',') {
            let name = import_item_name(item);
            if name.is_empty() || name == "*" {
                continue;
            }
            let import_path = format!("{}.{}", module.trim(), name);
            push_import_reference(
                &mut out,
                raw_line,
                line_index + 1,
                name,
                &import_path,
                search_start,
            );
        }
    }
    out
}

fn javascript_import_references(source: &str) -> Vec<CodeReference> {
    let mut out = Vec::new();
    let mut in_block_comment = false;
    for (line_index, raw_line) in source.lines().enumerate() {
        let cleaned = strip_c_like_block_comments(raw_line, &mut in_block_comment);
        let line = strip_line_comment(&cleaned, "//").trim();
        let Some(rest) = line.strip_prefix("import ") else {
            continue;
        };
        let Some((clause, from_part)) = rest.split_once(" from ") else {
            continue;
        };
        let Some(import_path) = quoted_import_path(from_part) else {
            continue;
        };
        let Some(group) = braced_segment(clause) else {
            continue;
        };
        let search_start = raw_line.find('{').map_or(0, |idx| idx + 1);
        for item in group.split(',') {
            let name = import_item_name(item);
            if !name.is_empty() {
                push_import_reference(
                    &mut out,
                    raw_line,
                    line_index + 1,
                    name,
                    import_path,
                    search_start,
                );
            }
        }
    }
    out
}

fn rust_use_body(line: &str) -> Option<&str> {
    let line = strip_visibility(line.trim());
    Some(line.strip_prefix("use ")?.trim_end_matches(';').trim())
}

/// Strip a leading `pub`, `pub(crate)`, `pub(super)`, `pub(in path)` visibility
/// qualifier. Returns the line unchanged when there is no such qualifier (so a
/// `published`-style identifier or a non-`use` line is left intact for the
/// caller's `strip_prefix("use ")` to reject).
fn strip_visibility(line: &str) -> &str {
    let Some(rest) = line.strip_prefix("pub") else {
        return line;
    };
    let after = match rest.chars().next() {
        Some('(') => {
            let Some(close) = rest.find(')') else {
                return line;
            };
            &rest[close + 1..]
        }
        Some(c) if c.is_whitespace() => rest,
        _ => return line, // e.g. `published` — not a visibility token
    };
    after.trim_start()
}

fn split_braced_import(body: &str) -> Option<(&str, &str)> {
    let start = body.find('{')?;
    let end = body.rfind('}')?;
    if end <= start {
        return None;
    }
    Some((
        body[..start].trim_end_matches("::").trim(),
        &body[start + 1..end],
    ))
}

fn braced_segment(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then_some(&text[start + 1..end])
}

fn push_group_imports(
    out: &mut Vec<CodeReference>,
    raw_line: &str,
    line: usize,
    prefix: &str,
    group: &str,
    separator: &str,
) {
    let search_start = raw_line.find('{').map_or(0, |idx| idx + 1);
    for item in group.split(',') {
        let name = import_item_name(item);
        if name.is_empty() || name == "*" || name == "self" {
            continue;
        }
        let import_path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}{separator}{name}")
        };
        push_import_reference(out, raw_line, line, name, &import_path, search_start);
    }
}

fn push_simple_import(
    out: &mut Vec<CodeReference>,
    raw_line: &str,
    line: usize,
    import_path: &str,
    separator: &str,
) {
    let Some(raw_name) = import_path.rsplit(separator).next().map(str::trim) else {
        return;
    };
    let name = import_item_name(raw_name);
    if name.is_empty() || name == "*" || name == "self" {
        return;
    }
    let resolved_import_path = import_path
        .rsplit_once(" as ")
        .map_or(import_path, |(source_path, _)| source_path.trim());
    let search_start = raw_line.rfind(name).unwrap_or(0);
    push_import_reference(
        out,
        raw_line,
        line,
        name,
        resolved_import_path,
        search_start,
    );
}

fn import_item_name(item: &str) -> &str {
    item.split(" as ").next().unwrap_or(item).trim()
}

fn quoted_import_path(text: &str) -> Option<&str> {
    let quote_index = text.find(['"', '\''])?;
    let quote = text.as_bytes().get(quote_index).copied()? as char;
    let rest = &text[quote_index + 1..];
    let end = rest.find(quote)?;
    Some(&rest[..end])
}

fn strip_line_comment<'a>(line: &'a str, marker: &str) -> &'a str {
    line.split_once(marker).map_or(line, |(before, _)| before)
}

fn strip_c_like_block_comments(line: &str, in_block: &mut bool) -> String {
    let mut rest = line;
    let mut out = String::new();
    loop {
        if *in_block {
            let Some(end) = rest.find("*/") else {
                return out;
            };
            rest = &rest[end + 2..];
            *in_block = false;
            continue;
        }
        let Some(start) = rest.find("/*") else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        *in_block = true;
    }
}

fn strip_python_triple_quotes(line: &str, active: &mut Option<&'static str>) -> String {
    let mut rest = line;
    let mut out = String::new();
    loop {
        if let Some(delimiter) = *active {
            let Some(end) = rest.find(delimiter) else {
                return out;
            };
            rest = &rest[end + delimiter.len()..];
            *active = None;
            continue;
        }
        let single = rest.find("'''").map(|idx| (idx, "'''"));
        let double = rest.find("\"\"\"").map(|idx| (idx, "\"\"\""));
        let Some((start, delimiter)) = earliest_delimiter(single, double) else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..start]);
        rest = &rest[start + delimiter.len()..];
        *active = Some(delimiter);
    }
}

fn earliest_delimiter(
    left: Option<(usize, &'static str)>,
    right: Option<(usize, &'static str)>,
) -> Option<(usize, &'static str)> {
    match (left, right) {
        (Some(left), Some(right)) if right.0 < left.0 => Some(right),
        (Some(left), _) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn push_import_reference(
    out: &mut Vec<CodeReference>,
    raw_line: &str,
    line: usize,
    name: &str,
    import_path: &str,
    search_start: usize,
) {
    let column = raw_line
        .get(search_start..)
        .and_then(|suffix| suffix.find(name).map(|idx| search_start + idx + 1))
        .or_else(|| raw_line.find(name).map(|idx| idx + 1))
        .unwrap_or(1);
    out.push(
        CodeReference::new("import", name, line)
            .with_column(column)
            .with_import_path(import_path),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import_named<'a>(refs: &'a [CodeReference], name: &str) -> Option<&'a CodeReference> {
        refs.iter().find(|r| r.kind == "import" && r.name == name)
    }

    #[test]
    fn visibility_qualified_use_reexports_are_captured() {
        for line in [
            "pub(crate) use foo::Bar;",
            "pub(super) use foo::Bar;",
            "pub(in crate::x) use foo::Bar;",
            "pub use foo::Bar;",
        ] {
            let refs = rust_import_references(line);
            let bar = import_named(&refs, "Bar")
                .unwrap_or_else(|| panic!("expected an import reference for `{line}`"));
            assert_eq!(
                bar.import_path.as_deref(),
                Some("foo::Bar"),
                "import_path for `{line}`"
            );
        }
    }

    #[test]
    fn visibility_qualified_non_use_lines_yield_no_import() {
        for line in [
            "pub fn thing() {}",
            "pub(crate) fn thing() {}",
            "pub(in crate::x) struct Thing;",
            "let published = 1;",
        ] {
            let refs = rust_import_references(line);
            assert!(
                refs.is_empty(),
                "expected no import references for `{line}`, got {refs:?}"
            );
        }
    }

    #[test]
    fn triple_quote_inside_comment_does_not_swallow_later_imports() {
        // The `'''` lives inside a `#` comment, so it must NOT open a docstring;
        // the following `from ... import ...` lines must still be captured.
        let source = "x = 1  # opening ''' quote\nfrom os import path\nfrom typing import List\n";
        let refs = python_import_references(source);
        let path = import_named(&refs, "path").expect("`from os import path` should be captured");
        assert_eq!(path.import_path.as_deref(), Some("os.path"));
        let list =
            import_named(&refs, "List").expect("`from typing import List` should be captured");
        assert_eq!(list.import_path.as_deref(), Some("typing.List"));
    }

    #[test]
    fn real_triple_quoted_docstring_still_suppresses_imports() {
        // A genuine multi-line docstring must still hide an enclosed import line.
        let source = "'''\nfrom os import path\n'''\nfrom typing import List\n";
        let refs = python_import_references(source);
        assert!(
            import_named(&refs, "path").is_none(),
            "import inside a real docstring must be suppressed: {refs:?}"
        );
        let list = import_named(&refs, "List")
            .expect("`from typing import List` after the docstring should be captured");
        assert_eq!(list.import_path.as_deref(), Some("typing.List"));
    }
}
