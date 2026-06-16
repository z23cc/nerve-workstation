use super::symbols::truncate_chars;
use super::*;

/// One structural match from [`ast_search`]: the `@match` region's line and
/// text, plus any other captures as metavariables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AstMatch {
    pub line: usize,
    pub text: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub captures: BTreeMap<String, String>,
}

/// Whether `language_name` maps to a supported tree-sitter grammar.
pub(crate) fn ast_language_supported(language_name: &str) -> bool {
    Language::from_name(language_name).is_some()
}

/// Display language name for a path's extension, if supported.
pub(crate) fn path_language_name(path: &str) -> Option<&'static str> {
    Language::from_path(path).map(Language::name)
}

pub(super) fn ast_node_text(node: tree_sitter::Node, source: &[u8]) -> String {
    let text = node.utf8_text(source).unwrap_or_default();
    let first = text.lines().next().unwrap_or(text);
    truncate_chars(first, 160)
}

/// Run a tree-sitter query over one source file. The `@match` capture (or, if
/// absent, the largest captured node) marks each result; other captures are
/// returned as metavariables. Errors if the language is unsupported or the query
/// is invalid for it.
pub(crate) fn ast_search(
    path: &str,
    source: &str,
    query_src: &str,
    max: usize,
) -> Result<Vec<AstMatch>, String> {
    let language = Language::from_path(path).ok_or_else(|| "unsupported language".to_string())?;
    let ts_language = language.ts_language();
    if query_src.contains("(#") {
        return Err(
            "query predicates such as (#eq? ...) are not applied yet; use a structural pattern"
                .to_string(),
        );
    }
    let query = tree_sitter::Query::new(&ts_language, query_src)
        .map_err(|err| format!("invalid query: {err}"))?;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_language)
        .map_err(|err| format!("language error: {err}"))?;
    let Some(tree) = parser.parse(source, None) else {
        return Ok(Vec::new());
    };
    let names = query.capture_names();
    let bytes = source.as_bytes();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut iter = cursor.matches(&query, tree.root_node(), bytes);
    let mut matches = Vec::new();
    while let Some(query_match) = iter.next() {
        let mut captures = BTreeMap::new();
        let mut match_node = None;
        for capture in query_match.captures {
            let name = names[capture.index as usize];
            if name == "match" {
                match_node = Some(capture.node);
            }
            captures.insert(name.to_string(), ast_node_text(capture.node, bytes));
        }
        let node = match_node.or_else(|| {
            query_match
                .captures
                .iter()
                .map(|capture| capture.node)
                .max_by_key(|node| node.byte_range().len())
        });
        let Some(node) = node else { continue };
        matches.push(AstMatch {
            line: node.start_position().row + 1,
            text: ast_node_text(node, bytes),
            captures,
        });
        if matches.len() >= max {
            break;
        }
    }
    Ok(matches)
}

/// Replace every `@match` region matched by `query_src` with `replacement`,
/// where `${capture}` placeholders are substituted by captured text. Returns the
/// new source and rewrite count. Applied bottom-up so byte offsets stay valid;
/// overlapping (nested) matches keep the outermost.
pub(crate) fn ast_rewrite(
    path: &str,
    source: &str,
    query_src: &str,
    replacement: &str,
) -> Result<(String, usize), String> {
    let language = Language::from_path(path).ok_or_else(|| "unsupported language".to_string())?;
    let ts_language = language.ts_language();
    if query_src.contains("(#") {
        return Err(
            "query predicates such as (#eq? ...) are not applied yet; use a structural pattern"
                .to_string(),
        );
    }
    let query = tree_sitter::Query::new(&ts_language, query_src)
        .map_err(|err| format!("invalid query: {err}"))?;
    let names = query.capture_names();
    if !names.contains(&"match") {
        return Err("rewrite query must capture the region to replace as @match".to_string());
    }
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_language)
        .map_err(|err| format!("language error: {err}"))?;
    let Some(tree) = parser.parse(source, None) else {
        return Ok((source.to_string(), 0));
    };
    let bytes = source.as_bytes();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut iter = cursor.matches(&query, tree.root_node(), bytes);
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    while let Some(query_match) = iter.next() {
        let mut values: BTreeMap<&str, &str> = BTreeMap::new();
        let mut region = None;
        for capture in query_match.captures {
            let name = names[capture.index as usize];
            let text = capture.node.utf8_text(bytes).unwrap_or_default();
            if name == "match" {
                region = Some(capture.node.byte_range());
            }
            values.insert(name, text);
        }
        let Some(range) = region else { continue };
        edits.push((range, render_template(replacement, &values)));
    }
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.0.start));
    let mut result = source.to_string();
    let mut boundary = result.len();
    let mut count = 0;
    for (range, rendered) in edits {
        if range.end > boundary {
            continue;
        }
        result.replace_range(range.clone(), &rendered);
        boundary = range.start;
        count += 1;
    }
    Ok((result, count))
}

pub(super) fn render_template(template: &str, values: &BTreeMap<&str, &str>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(idx) = rest.find("${") {
        out.push_str(&rest[..idx]);
        rest = &rest[idx + 2..];
        match rest.find('}') {
            Some(end) => {
                if let Some(value) = values.get(&rest[..end]) {
                    out.push_str(value);
                }
                rest = &rest[end + 1..];
            }
            None => {
                out.push_str("${");
                break;
            }
        }
    }
    out.push_str(rest);
    out
}
