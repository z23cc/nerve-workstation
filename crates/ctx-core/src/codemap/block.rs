use super::*;

/// First descendant (document order) whose kind is in `kinds`.
pub(super) fn first_descendant_kind<'a>(
    node: tree_sitter::Node<'a>,
    kinds: &[&str],
) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if kinds.contains(&child.kind()) {
            return Some(child);
        }
        if let Some(found) = first_descendant_kind(child, kinds) {
            return Some(found);
        }
    }
    None
}

/// Resolve the syntactic block beginning on 1-based `start_line` using
/// tree-sitter: the largest named node that starts on that line (so a `def`/`fn`
/// opener selects the whole construct, and pointing at a leading decorator sweeps
/// it in too). Returns an inclusive 1-based `(start, end)` range, or `None` when
/// the language is unsupported or nothing begins there — callers then fall back
/// to a brace/indentation heuristic.
pub(crate) fn block_span(path: &str, source: &str, start_line: usize) -> Option<(usize, usize)> {
    let language = Language::from_path(path)?;
    let target_row = start_line.checked_sub(1)?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language.ts_language()).ok()?;
    let tree = parser.parse(source, None)?;

    // Seed from the root's children so the whole-file root node (which also
    // starts on line 1) is never chosen as the block.
    let root = tree.root_node();
    let mut best: Option<tree_sitter::Node> = None;
    let mut stack: Vec<tree_sitter::Node> = Vec::new();
    let mut root_cursor = root.walk();
    for child in root.children(&mut root_cursor) {
        if child.start_position().row <= target_row && child.end_position().row >= target_row {
            stack.push(child);
        }
    }
    while let Some(node) = stack.pop() {
        if node.is_named()
            && node.start_position().row == target_row
            && best.is_none_or(|current| node.end_byte() > current.end_byte())
        {
            best = Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.start_position().row <= target_row && child.end_position().row >= target_row {
                stack.push(child);
            }
        }
    }

    let node = best?;
    let mut end_row = node.end_position().row;
    // A node ending at column 0 closes on the previous line, not the next.
    if node.end_position().column == 0 && end_row > target_row {
        end_row -= 1;
    }
    Some((start_line, end_row + 1))
}

/// A tree-sitter syntax problem in a file (used to flag broken edits).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntaxIssue {
    pub line: usize,
    pub message: String,
}

/// Re-parse `source` and report tree-sitter `ERROR` / missing nodes as syntax
/// diagnostics. Empty for unsupported languages or a clean parse. Syntax-level
/// only — this is not type checking or a language server.
pub(crate) fn syntax_diagnostics(path: &str, source: &str) -> Vec<SyntaxIssue> {
    const MAX_ISSUES: usize = 20;
    let Some(language) = Language::from_path(path) else {
        return Vec::new();
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language.ts_language()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    if !tree.root_node().has_error() {
        return Vec::new();
    }

    let mut issues = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.is_missing() {
            issues.push(SyntaxIssue {
                line: node.start_position().row + 1,
                message: format!("missing {}", node.kind()),
            });
        } else if node.is_error() {
            issues.push(SyntaxIssue {
                line: node.start_position().row + 1,
                message: "syntax error".to_string(),
            });
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.has_error() {
                stack.push(child);
            }
        }
    }
    issues.sort_by_key(|issue| issue.line);
    issues.dedup();
    issues.truncate(MAX_ISSUES);
    issues
}
