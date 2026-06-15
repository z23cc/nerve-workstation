//! Multi-language codemap extraction via tree-sitter tag queries.
//!
//! One engine (tree-sitter) plus each grammar's `tags.scm` query produces
//! definitions (codemap symbols) and references (consumed by repo-map). Adding a
//! language is a grammar crate + its tags query. Note: tree-sitter grammars are
//! C, so a C toolchain is required at build time.

use crate::{models::*, port::CatalogProvider, snapshot::CatalogSnapshot};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::PathBuf, sync::OnceLock};
use tree_sitter_tags::{TagsConfiguration, TagsContext};

/// A symbol (definition) extracted from a source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeSymbol {
    pub kind: String,
    pub name: String,
    pub line: usize,
    /// Declaration signature (e.g. `pub fn foo(a: A) -> B`), body excluded.
    /// `None` when extraction yields nothing useful.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// An AST-derived reference occurrence used internally by repo-map.
///
/// References are name-level occurrences (calls, etc.) captured by the tags
/// query. They do not perform scope/type/alias resolution; repo-map resolves
/// them later by same-language name matching against definitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeReference {
    pub kind: String,
    pub name: String,
    pub line: usize,
    pub import_path: Option<String>,
}

/// Parsed code facts for one source file. Public codemap responses expose only
/// `symbols`; repo-map consumes `references` from the same parse result so files
/// are not parsed twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedCodeFile {
    pub language: String,
    pub symbols: Vec<CodeSymbol>,
    pub references: Vec<CodeReference>,
}

/// Symbols for one cataloged file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileCodeStructure {
    pub path: String,
    pub language: String,
    pub symbols: Vec<CodeSymbol>,
}

/// Non-fatal codemap diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeStructureDiagnostic {
    pub path: Option<String>,
    pub message: String,
}

/// Response for `get_code_structure`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeStructureResponse {
    pub files: Vec<FileCodeStructure>,
    pub diagnostics: Vec<CodeStructureDiagnostic>,
    pub omitted: usize,
}

/// Extract code structure for selected paths.
///
/// Empty `paths` means the whole catalog. Directory paths select entries by
/// prefix; file paths select exact entries. Unsupported files are omitted.
pub fn get_code_structure<P: CatalogProvider>(
    provider: &P,
    snapshot: &CatalogSnapshot,
    paths: &[PathBuf],
) -> Result<CodeStructureResponse, CtxError> {
    let selected = select_entries(snapshot, paths);
    let mut files = Vec::new();
    let mut diagnostics = Vec::new();
    let mut omitted = 0usize;

    for entry in selected {
        match provider.code_symbols_for_path(&entry.abs_path, &entry.rel_path)? {
            Ok(Some(parsed)) => files.push(FileCodeStructure {
                path: entry.rel_path.clone(),
                language: parsed.language.clone(),
                symbols: parsed.symbols.clone(),
            }),
            Ok(None) => omitted += 1,
            Err(message) => diagnostics.push(CodeStructureDiagnostic {
                path: Some(entry.rel_path.clone()),
                message,
            }),
        }
    }

    Ok(CodeStructureResponse {
        files,
        diagnostics,
        omitted,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
    Java,
    C,
    Cpp,
    CSharp,
    Ruby,
    Php,
}

impl Language {
    fn from_path(path: &str) -> Option<Self> {
        let lower = path.to_ascii_lowercase();
        let ext = lower.rsplit('.').next().unwrap_or("");
        Some(match ext {
            "rs" => Self::Rust,
            "py" | "pyi" => Self::Python,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "ts" | "mts" | "cts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "go" => Self::Go,
            "java" => Self::Java,
            "c" | "h" => Self::C,
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Self::Cpp,
            "cs" => Self::CSharp,
            "rb" => Self::Ruby,
            "php" | "phtml" => Self::Php,
            _ => return None,
        })
    }

    /// Display language tag for the response `language` field. Repo-map keeps
    /// JS/TS/TSX in one resolution family separately (see repomap language_family).
    fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::Go => "go",
            Self::Java => "java",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::CSharp => "csharp",
            Self::Ruby => "ruby",
            Self::Php => "php",
        }
    }

    /// Cached tags configuration (compiling a query is expensive).
    fn config(self) -> Option<&'static TagsConfiguration> {
        fn build(language: tree_sitter::Language, query: &str) -> Option<TagsConfiguration> {
            TagsConfiguration::new(language, query, "").ok()
        }
        macro_rules! cached {
            ($lang:expr, $query:expr) => {{
                static CELL: OnceLock<Option<TagsConfiguration>> = OnceLock::new();
                CELL.get_or_init(|| build($lang.into(), $query)).as_ref()
            }};
        }
        match self {
            Self::Rust => cached!(tree_sitter_rust::LANGUAGE, tree_sitter_rust::TAGS_QUERY),
            Self::Python => cached!(tree_sitter_python::LANGUAGE, tree_sitter_python::TAGS_QUERY),
            Self::JavaScript => {
                cached!(
                    tree_sitter_javascript::LANGUAGE,
                    tree_sitter_javascript::TAGS_QUERY
                )
            }
            // TypeScript's tags.scm only adds TS-specific captures and inherits the
            // rest from JavaScript; concatenate both so class/function decls are seen.
            Self::TypeScript => cached!(
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
                &format!(
                    "{}\n{}",
                    tree_sitter_javascript::TAGS_QUERY,
                    tree_sitter_typescript::TAGS_QUERY
                )
            ),
            Self::Tsx => cached!(
                tree_sitter_typescript::LANGUAGE_TSX,
                &format!(
                    "{}\n{}",
                    tree_sitter_javascript::TAGS_QUERY,
                    tree_sitter_typescript::TAGS_QUERY
                )
            ),
            Self::Go => cached!(tree_sitter_go::LANGUAGE, tree_sitter_go::TAGS_QUERY),
            Self::Java => cached!(tree_sitter_java::LANGUAGE, tree_sitter_java::TAGS_QUERY),
            Self::C => cached!(tree_sitter_c::LANGUAGE, tree_sitter_c::TAGS_QUERY),
            Self::Cpp => cached!(tree_sitter_cpp::LANGUAGE, tree_sitter_cpp::TAGS_QUERY),
            Self::CSharp => cached!(
                tree_sitter_c_sharp::LANGUAGE,
                tree_sitter_c_sharp::TAGS_QUERY
            ),
            Self::Ruby => cached!(tree_sitter_ruby::LANGUAGE, tree_sitter_ruby::TAGS_QUERY),
            Self::Php => cached!(tree_sitter_php::LANGUAGE_PHP, tree_sitter_php::TAGS_QUERY),
        }
    }
}

pub(crate) fn symbols_for_path(
    source: &str,
    rel_path: &str,
) -> Result<Option<ParsedCodeFile>, String> {
    let Some(language) = Language::from_path(rel_path) else {
        return Ok(None);
    };
    let (symbols, references) = code_facts_for_language(language, source)?;
    Ok(Some(ParsedCodeFile {
        language: language.name().to_string(),
        symbols,
        references,
    }))
}

#[cfg(fuzzing)]
#[doc(hidden)]
pub fn fuzz_symbols_for_path(
    source: &str,
    rel_path: &str,
) -> Result<Option<(String, Vec<CodeSymbol>)>, String> {
    Ok(symbols_for_path(source, rel_path)?.map(|parsed| (parsed.language, parsed.symbols)))
}

fn code_facts_for_language(
    language: Language,
    source: &str,
) -> Result<(Vec<CodeSymbol>, Vec<CodeReference>), String> {
    let config = language
        .config()
        .ok_or_else(|| format!("no tags configuration for {}", language.name()))?;
    let mut context = TagsContext::new();
    let bytes = source.as_bytes();
    let (tags, _failed) = context
        .generate_tags(config, bytes, None)
        .map_err(|err| format!("tags error: {err:?}"))?;

    let mut symbols = Vec::new();
    let mut references = Vec::new();
    for tag in tags {
        let tag = tag.map_err(|err| format!("tag error: {err:?}"))?;
        let Some(name) = bytes.get(tag.name_range.clone()) else {
            continue;
        };
        let name = String::from_utf8_lossy(name).into_owned();
        let line = tag.span.start.row + 1;
        let kind = config.syntax_type_name(tag.syntax_type_id).to_string();
        if tag.is_definition {
            let signature = signature_for(language, bytes, &tag);
            symbols.push(CodeSymbol {
                kind,
                name,
                line,
                signature,
            });
        } else {
            references.push(CodeReference {
                kind,
                name,
                line,
                import_path: None,
            });
        }
    }
    // Deterministic order for stable output and goldens.
    symbols.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.name.cmp(&b.name)));
    Ok((symbols, references))
}

/// Build a compact one-line signature for a definition tag: text from the start
/// of the definition node up to the body (a top-level `{` or `;` for brace
/// languages, `:` for Python), with all whitespace collapsed to single spaces and
/// the length capped. Falls back to the trimmed first line tree-sitter already
/// computed (`line_range`). Returns `None` if nothing useful remains.
fn signature_for(language: Language, source: &[u8], tag: &tree_sitter_tags::Tag) -> Option<String> {
    const MAX_SCAN: usize = 512;
    const MAX_CHARS: usize = 200;

    let start = tag.range.start.min(source.len());
    let scan_end = tag.range.end.min(start + MAX_SCAN).min(source.len());
    let node = source.get(start..scan_end)?;
    let raw = match signature_span_end(language, node) {
        Some(end) => &node[..end],
        None => {
            let lo = tag.line_range.start.min(source.len());
            let hi = tag.line_range.end.min(source.len());
            source.get(lo..hi)?
        }
    };

    let collapsed = String::from_utf8_lossy(raw)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = collapsed
        .trim()
        .trim_end_matches(['{', ':', ';'])
        .trim_end();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() > MAX_CHARS {
        let capped: String = trimmed.chars().take(MAX_CHARS - 1).collect();
        Some(format!("{capped}\u{2026}"))
    } else {
        Some(trimmed.to_string())
    }
}

/// Byte offset within `node` where a definition's signature ends (exclusive):
/// the first top-level body opener. Brace languages stop before `{` or at `;`;
/// Python stops at the header `:`. Bracket depth (`()`/`[]`, plus `{}` for Python)
/// keeps separators inside argument lists from ending the signature.
fn signature_span_end(language: Language, node: &[u8]) -> Option<usize> {
    let python = matches!(language, Language::Python);
    let mut depth: i32 = 0;
    for (i, &byte) in node.iter().enumerate() {
        match byte {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'{' if python => depth += 1,
            b'}' if python => depth -= 1,
            b'{' if depth <= 0 => return Some(i),
            b';' if depth <= 0 => return Some(i),
            b':' if python && depth <= 0 => return Some(i),
            _ => {}
        }
    }
    None
}

fn select_entries<'a>(
    snapshot: &'a CatalogSnapshot,
    paths: &[PathBuf],
) -> Vec<&'a crate::models::CatalogEntry> {
    if paths.is_empty() {
        return snapshot.entries.iter().collect();
    }

    let mut selected = BTreeSet::new();
    for path in paths {
        let raw = path.to_string_lossy().replace('\\', "/");
        let rel = raw.trim_start_matches("./").trim_end_matches('/');
        let canonical = path.canonicalize().ok();
        for (idx, entry) in snapshot.entries.iter().enumerate() {
            let rel_match = rel.is_empty()
                || entry.rel_path == rel
                || entry.rel_path.starts_with(&format!("{rel}/"));
            let abs_match = canonical
                .as_ref()
                .is_some_and(|abs| entry.abs_path == *abs || entry.abs_path.starts_with(abs));
            if rel_match || abs_match {
                selected.insert(idx);
            }
        }
    }

    selected
        .into_iter()
        .map(|idx| &snapshot.entries[idx])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str, rel_path: &str) -> ParsedCodeFile {
        symbols_for_path(source, rel_path)
            .expect("parse")
            .expect("supported language")
    }

    fn has_symbol(parsed: &ParsedCodeFile, name: &str) -> bool {
        parsed.symbols.iter().any(|symbol| symbol.name == name)
    }

    #[test]
    fn rust_definitions_and_references() {
        let parsed = parse(
            "pub struct Widget;\npub fn make_widget() -> Widget { Widget }\n",
            "lib.rs",
        );
        assert_eq!(parsed.language, "rust");
        assert!(has_symbol(&parsed, "Widget"));
        assert!(has_symbol(&parsed, "make_widget"));
    }

    #[test]
    fn python_definitions() {
        let parsed = parse(include_str!("../tests/fixtures/gamma.py"), "gamma.py");
        assert_eq!(parsed.language, "python");
        assert!(has_symbol(&parsed, "PyAlpha"));
        assert!(has_symbol(&parsed, "py_helper"));
    }

    #[test]
    fn javascript_definitions() {
        let parsed = parse(include_str!("../tests/fixtures/delta.js"), "delta.js");
        assert_eq!(parsed.language, "javascript");
        assert!(has_symbol(&parsed, "Widget"));
    }

    #[test]
    fn go_definitions_and_references() {
        let parsed = parse(include_str!("../tests/go_fixture.go"), "main.go");
        assert_eq!(parsed.language, "go");
        assert!(has_symbol(&parsed, "NewService"));
        assert!(has_symbol(&parsed, "Greet"));
        assert!(!parsed.references.is_empty());
    }

    #[test]
    fn typescript_definitions() {
        let parsed = parse(
            "export class Service {\n  greet(name: string): string { return name; }\n}\nexport function make(): Service { return new Service(); }\n",
            "svc.ts",
        );
        assert_eq!(parsed.language, "typescript");
        assert!(
            has_symbol(&parsed, "Service"),
            "symbols: {:?}",
            parsed.symbols
        );
        assert!(has_symbol(&parsed, "make"));
    }

    #[test]
    fn symbols_include_declaration_signatures() {
        let parsed = parse(
            "pub fn add(\n    left: usize,\n    right: usize,\n) -> usize {\n    left + right\n}\n",
            "math.rs",
        );
        let add = parsed
            .symbols
            .iter()
            .find(|symbol| symbol.name == "add")
            .expect("add symbol");
        let signature = add.signature.as_deref().unwrap_or_default();
        assert!(
            signature.starts_with("pub fn add("),
            "signature: {signature}"
        );
        assert!(signature.contains("-> usize"), "signature: {signature}");
        assert!(
            !signature.contains('{'),
            "signature must stop before the body: {signature}"
        );
    }

    #[test]
    fn java_definitions() {
        let parsed = parse(
            "class Greeter {\n  public String greet(String name) { return name; }\n}\n",
            "Greeter.java",
        );
        assert_eq!(parsed.language, "java");
        assert!(has_symbol(&parsed, "Greeter"));
        assert!(has_symbol(&parsed, "greet"));
    }

    #[test]
    fn ruby_definitions() {
        let parsed = parse(
            "class Greeter\n  def greet(name)\n    name\n  end\nend\n",
            "greeter.rb",
        );
        assert_eq!(parsed.language, "ruby");
        assert!(has_symbol(&parsed, "Greeter"));
        assert!(has_symbol(&parsed, "greet"));
    }

    #[test]
    fn unsupported_extension_returns_none() {
        assert!(
            symbols_for_path("plain text", "notes.txt")
                .expect("ok")
                .is_none()
        );
    }
}
