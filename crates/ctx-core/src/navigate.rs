//! Deterministic, syntax-level symbol navigation.
//!
//! Built on the same tree-sitter tag extraction that powers the repo-map, this
//! is **search-based** navigation in the sense of ctags / tree-sitter code
//! navigation: a symbol name is matched against the definition and reference
//! tags collected across the catalog. It is **not** a scope/type resolver, so
//! results may include false positives (an unrelated symbol of the same name)
//! and miss aliased or re-exported bindings. The upside is that it needs no
//! language server and no external process, runs over all 11 supported
//! languages, and is fully deterministic — the right tradeoff for an embeddable,
//! reproducible backend (the same choice GitHub makes for code navigation at
//! scale).

use crate::{
    cancel::CancelToken,
    codemap::block_span,
    models::CtxError,
    port::CatalogProvider,
    repomap::{indexed_files_cancellable, resolve_import_reference},
    snapshot::CatalogSnapshot,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

/// Caveat surfaced on every navigation response so callers (and models) know the
/// results are syntactic name matches, not compiler-accurate resolution.
const NAV_NOTE: &str = "syntax-level name match across the catalog; not a scope/type resolver, so results may include unrelated same-name symbols and miss aliases or re-exports";

/// Request for `goto_definition` / `find_references`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NavigateRequest {
    /// Exact symbol name to look up (case-sensitive).
    pub symbol: String,
    /// Restrict results to this display language (e.g. `rust`, `typescript`,
    /// `tsx`). `None` searches every language.
    #[serde(default)]
    pub language: Option<String>,
    /// `find_references` only: also return the symbol's definitions.
    #[serde(default)]
    pub include_definitions: bool,
    /// `find_references` only: drop low-confidence (ambiguous name-only) hits.
    #[serde(default)]
    pub confident_only: bool,
    /// Maximum locations returned per bucket.
    pub max_results: usize,
}

/// How trustworthy a syntactic reference match is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// The name is unambiguous (single definition), the reference shares a file
    /// with a definition, or its file imports a defining file.
    High,
    /// Name-only match while multiple definitions of that name exist elsewhere.
    Low,
}

/// One definition site for a symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolLocation {
    pub path: String,
    pub display_path: String,
    pub line: usize,
    pub kind: String,
    pub language: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Trimmed source line at `line`, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// One reference site for a symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceLocation {
    pub path: String,
    pub display_path: String,
    pub line: usize,
    pub kind: String,
    pub language: String,
    /// Trimmed source line at `line`, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Whether this name match is unambiguous / import-backed (`high`) or a
    /// name-only match while the name is defined in multiple places (`low`).
    pub confidence: Confidence,
}

/// Response for `goto_definition`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefinitionResponse {
    pub symbol: String,
    pub definitions: Vec<SymbolLocation>,
    pub total: usize,
    pub truncated: bool,
    pub note: String,
}

/// Response for `find_references`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferencesResponse {
    pub symbol: String,
    pub references: Vec<ReferenceLocation>,
    /// Populated only when `include_definitions` is set.
    pub definitions: Vec<SymbolLocation>,
    /// Number of definition sites of this name (in scope); >1 means the name is
    /// ambiguous, so low-confidence references may belong to a different symbol.
    pub definition_count: usize,
    pub total: usize,
    pub truncated: bool,
    pub note: String,
}

/// Direction of a `call_hierarchy` query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallDirection {
    /// Callers of the symbol (who references it, by enclosing definition).
    Incoming,
    /// Callees of the symbol (what its body references, resolved to definitions).
    Outgoing,
    /// Both directions.
    Both,
}

/// Request for `call_hierarchy`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CallHierarchyRequest {
    pub symbol: String,
    pub direction: CallDirection,
    #[serde(default)]
    pub language: Option<String>,
    pub max_results: usize,
}

/// One related symbol in a call hierarchy (a caller for incoming, a callee for
/// outgoing).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallEdge {
    /// The related symbol's name.
    pub symbol: String,
    pub path: String,
    pub display_path: String,
    pub line: usize,
    pub kind: String,
    pub language: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Response for `call_hierarchy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallHierarchyResponse {
    pub symbol: String,
    /// Callers (populated for `incoming`/`both`).
    pub incoming: Vec<CallEdge>,
    /// Callees (populated for `outgoing`/`both`).
    pub outgoing: Vec<CallEdge>,
    pub note: String,
}

fn language_matches(filter: Option<&str>, language: &str) -> bool {
    filter.is_none_or(|wanted| wanted == language)
}

/// Lazily reads file sources (once per path) to supply line snippets and full
/// source text for tree-sitter block-span resolution.
struct Sources<'a, P: CatalogProvider + ?Sized> {
    provider: &'a P,
    cache: HashMap<String, Option<String>>,
}

impl<'a, P: CatalogProvider + ?Sized> Sources<'a, P> {
    fn new(provider: &'a P) -> Self {
        Self {
            provider,
            cache: HashMap::new(),
        }
    }

    fn source(&mut self, rel_path: &str, abs_path: &Path) -> Option<&str> {
        self.cache
            .entry(rel_path.to_string())
            .or_insert_with(|| {
                self.provider
                    .read_bytes(abs_path)
                    .ok()
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            })
            .as_deref()
    }

    /// Trimmed 1-based `line` from `rel_path`, or `None` if unavailable/blank.
    fn line(&mut self, rel_path: &str, abs_path: &Path, line: usize) -> Option<String> {
        let source = self.source(rel_path, abs_path)?;
        let text = source.lines().nth(line.checked_sub(1)?)?.trim();
        (!text.is_empty()).then(|| text.to_string())
    }
}

fn sort_locations(locations: &mut [SymbolLocation]) {
    locations.sort_by(|a, b| {
        a.display_path
            .cmp(&b.display_path)
            .then(a.line.cmp(&b.line))
            .then(a.kind.cmp(&b.kind))
    });
}

/// Find all definitions of `symbol` across the catalog.
pub fn goto_definition<P: CatalogProvider + Sync>(
    provider: &P,
    snapshot: &CatalogSnapshot,
    request: &NavigateRequest,
) -> Result<DefinitionResponse, CtxError> {
    goto_definition_cancellable(provider, snapshot, request, &CancelToken::never())
}

/// Cancellable [`goto_definition`].
pub fn goto_definition_cancellable<P: CatalogProvider + Sync>(
    provider: &P,
    snapshot: &CatalogSnapshot,
    request: &NavigateRequest,
    cancel: &CancelToken,
) -> Result<DefinitionResponse, CtxError> {
    let files = indexed_files_cancellable(provider, snapshot, cancel)?;
    let mut sources = Sources::new(provider);
    let mut definitions = Vec::new();
    for file in &files {
        if !language_matches(request.language.as_deref(), &file.language) {
            continue;
        }
        for symbol in &file.symbols {
            if symbol.name == request.symbol {
                let text = sources.line(&file.path, &file.abs_path, symbol.line);
                definitions.push(SymbolLocation {
                    path: file.path.clone(),
                    display_path: file.display_path.clone(),
                    line: symbol.line,
                    kind: symbol.kind.clone(),
                    language: file.language.clone(),
                    signature: symbol.signature.clone(),
                    text,
                });
            }
        }
    }
    sort_locations(&mut definitions);
    let total = definitions.len();
    let truncated = total > request.max_results;
    definitions.truncate(request.max_results);
    Ok(DefinitionResponse {
        symbol: request.symbol.clone(),
        definitions,
        total,
        truncated,
        note: NAV_NOTE.to_string(),
    })
}

/// Find all references to `symbol` across the catalog.
pub fn find_references<P: CatalogProvider + Sync>(
    provider: &P,
    snapshot: &CatalogSnapshot,
    request: &NavigateRequest,
) -> Result<ReferencesResponse, CtxError> {
    find_references_cancellable(provider, snapshot, request, &CancelToken::never())
}

/// Cancellable [`find_references`].
pub fn find_references_cancellable<P: CatalogProvider + Sync>(
    provider: &P,
    snapshot: &CatalogSnapshot,
    request: &NavigateRequest,
    cancel: &CancelToken,
) -> Result<ReferencesResponse, CtxError> {
    let files = indexed_files_cancellable(provider, snapshot, cancel)?;
    let mut sources = Sources::new(provider);
    let lang = request.language.as_deref();

    // Files (by index) that define this name, in scope — the basis for scoring
    // each reference's confidence.
    let def_files: HashSet<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, f)| language_matches(lang, &f.language))
        .filter(|(_, f)| f.symbols.iter().any(|s| s.name == request.symbol))
        .map(|(idx, _)| idx)
        .collect();
    // Definition sites (symbols), for the ambiguity indicator.
    let definition_count: usize = files
        .iter()
        .filter(|f| language_matches(lang, &f.language))
        .map(|f| {
            f.symbols
                .iter()
                .filter(|s| s.name == request.symbol)
                .count()
        })
        .sum();
    let unambiguous = definition_count <= 1;

    // A referencing file is high-confidence if it also defines the name or
    // imports a file that does (reusing the repo-map import resolver).
    let imports_a_definer = |idx: usize| -> bool {
        files[idx].references.iter().any(|reference| {
            reference.kind == "import"
                && resolve_import_reference(&files, idx, reference)
                    .is_some_and(|target| def_files.contains(&target))
        })
    };

    let mut references = Vec::new();
    for (idx, file) in files.iter().enumerate() {
        if !language_matches(lang, &file.language) {
            continue;
        }
        let file_is_confident = unambiguous || def_files.contains(&idx) || imports_a_definer(idx);
        let confidence = if file_is_confident {
            Confidence::High
        } else {
            Confidence::Low
        };
        for reference in &file.references {
            if reference.name == request.symbol {
                if request.confident_only && confidence == Confidence::Low {
                    continue;
                }
                let text = sources.line(&file.path, &file.abs_path, reference.line);
                references.push(ReferenceLocation {
                    path: file.path.clone(),
                    display_path: file.display_path.clone(),
                    line: reference.line,
                    kind: reference.kind.clone(),
                    language: file.language.clone(),
                    text,
                    confidence,
                });
            }
        }
    }
    references.sort_by(|a, b| {
        a.display_path
            .cmp(&b.display_path)
            .then(a.line.cmp(&b.line))
            .then(a.kind.cmp(&b.kind))
    });
    let total = references.len();
    let truncated = total > request.max_results;
    references.truncate(request.max_results);

    let definitions = if request.include_definitions {
        let mut defs = Vec::new();
        for file in &files {
            if !language_matches(request.language.as_deref(), &file.language) {
                continue;
            }
            for symbol in &file.symbols {
                if symbol.name == request.symbol {
                    let text = sources.line(&file.path, &file.abs_path, symbol.line);
                    defs.push(SymbolLocation {
                        path: file.path.clone(),
                        display_path: file.display_path.clone(),
                        line: symbol.line,
                        kind: symbol.kind.clone(),
                        language: file.language.clone(),
                        signature: symbol.signature.clone(),
                        text,
                    });
                }
            }
        }
        sort_locations(&mut defs);
        defs.truncate(request.max_results);
        defs
    } else {
        Vec::new()
    };

    Ok(ReferencesResponse {
        symbol: request.symbol.clone(),
        references,
        definitions,
        definition_count,
        total,
        truncated,
        note: NAV_NOTE.to_string(),
    })
}

/// Build a name-based call hierarchy for `symbol`.
pub fn call_hierarchy<P: CatalogProvider + Sync>(
    provider: &P,
    snapshot: &CatalogSnapshot,
    request: &CallHierarchyRequest,
) -> Result<CallHierarchyResponse, CtxError> {
    call_hierarchy_cancellable(provider, snapshot, request, &CancelToken::never())
}

/// Cancellable [`call_hierarchy`].
///
/// Incoming: every reference to `symbol`, mapped to its enclosing definition
/// (the innermost symbol whose tree-sitter block span contains the reference
/// line) — the caller. Outgoing: the references inside `symbol`'s own block,
/// resolved by name to definitions — the callees. Both are name-based and
/// best-effort (see the response note).
pub fn call_hierarchy_cancellable<P: CatalogProvider + Sync>(
    provider: &P,
    snapshot: &CatalogSnapshot,
    request: &CallHierarchyRequest,
    cancel: &CancelToken,
) -> Result<CallHierarchyResponse, CtxError> {
    let files = indexed_files_cancellable(provider, snapshot, cancel)?;
    let mut sources = Sources::new(provider);
    let lang = request.language.as_deref();

    // Span cache: (rel_path, symbol_line) -> Option<(start, end)>.
    let mut spans: HashMap<(String, usize), Option<(usize, usize)>> = HashMap::new();
    let mut span_of = |sources: &mut Sources<P>, rel: &str, abs: &Path, line: usize| {
        *spans.entry((rel.to_string(), line)).or_insert_with(|| {
            sources
                .source(rel, abs)
                .and_then(|src| block_span(rel, src, line))
        })
    };

    let want_incoming = matches!(
        request.direction,
        CallDirection::Incoming | CallDirection::Both
    );
    let want_outgoing = matches!(
        request.direction,
        CallDirection::Outgoing | CallDirection::Both
    );

    let mut incoming = Vec::new();
    if want_incoming {
        cancel.check_cancelled()?;
        for file in &files {
            if !language_matches(lang, &file.language) {
                continue;
            }
            for reference in &file.references {
                if reference.name != request.symbol {
                    continue;
                }
                // Enclosing symbol = smallest block span that contains the ref line,
                // among symbols declared at or before it.
                let mut best: Option<&crate::codemap::CodeSymbol> = None;
                let mut best_size = usize::MAX;
                for symbol in &file.symbols {
                    if symbol.line > reference.line {
                        continue;
                    }
                    if let Some((start, end)) =
                        span_of(&mut sources, &file.path, &file.abs_path, symbol.line)
                        && start <= reference.line
                        && reference.line <= end
                        && (end - start) < best_size
                    {
                        best_size = end - start;
                        best = Some(symbol);
                    }
                }
                if let Some(caller) = best {
                    let text = sources.line(&file.path, &file.abs_path, caller.line);
                    incoming.push(CallEdge {
                        symbol: caller.name.clone(),
                        path: file.path.clone(),
                        display_path: file.display_path.clone(),
                        line: caller.line,
                        kind: caller.kind.clone(),
                        language: file.language.clone(),
                        text,
                    });
                }
            }
        }
        incoming.sort_by(call_edge_cmp);
        incoming.dedup();
        incoming.truncate(request.max_results);
    }

    let mut outgoing = Vec::new();
    if want_outgoing {
        cancel.check_cancelled()?;
        // Definition index by name for resolving callees.
        let mut defs_by_name: HashMap<&str, Vec<DefRef>> = HashMap::new();
        for file in &files {
            for symbol in &file.symbols {
                defs_by_name
                    .entry(symbol.name.as_str())
                    .or_default()
                    .push(DefRef {
                        path: &file.path,
                        display_path: &file.display_path,
                        language: &file.language,
                        line: symbol.line,
                        kind: &symbol.kind,
                    });
            }
        }

        for file in &files {
            if !language_matches(lang, &file.language) {
                continue;
            }
            for symbol in &file.symbols {
                if symbol.name != request.symbol {
                    continue;
                }
                let Some((start, end)) =
                    span_of(&mut sources, &file.path, &file.abs_path, symbol.line)
                else {
                    continue;
                };
                let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
                for reference in &file.references {
                    if reference.line < start
                        || reference.line > end
                        || reference.name == request.symbol
                        || !seen.insert(reference.name.as_str())
                    {
                        continue;
                    }
                    if let Some(targets) = defs_by_name.get(reference.name.as_str()) {
                        for target in targets {
                            outgoing.push(CallEdge {
                                symbol: reference.name.clone(),
                                path: target.path.to_string(),
                                display_path: target.display_path.to_string(),
                                line: target.line,
                                kind: target.kind.to_string(),
                                language: target.language.to_string(),
                                text: None,
                            });
                        }
                    }
                }
            }
        }
        outgoing.sort_by(call_edge_cmp);
        outgoing.dedup();
        // Fill snippets after dedup to avoid redundant reads.
        for edge in &mut outgoing {
            if let Some(abs) = files
                .iter()
                .find(|f| f.path == edge.path)
                .map(|f| f.abs_path.clone())
            {
                edge.text = sources.line(&edge.path, &abs, edge.line);
            }
        }
        outgoing.truncate(request.max_results);
    }

    Ok(CallHierarchyResponse {
        symbol: request.symbol.clone(),
        incoming,
        outgoing,
        note: NAV_NOTE.to_string(),
    })
}

/// A definition reference into the indexed file set, for callee resolution.
struct DefRef<'a> {
    path: &'a str,
    display_path: &'a str,
    language: &'a str,
    line: usize,
    kind: &'a str,
}

fn call_edge_cmp(a: &CallEdge, b: &CallEdge) -> std::cmp::Ordering {
    a.display_path
        .cmp(&b.display_path)
        .then(a.line.cmp(&b.line))
        .then(a.symbol.cmp(&b.symbol))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FsCatalogProvider, RootPolicy, ScanOptions};
    use std::fs;

    fn temp_provider(
        files: &[(&str, &str)],
    ) -> (tempfile::TempDir, FsCatalogProvider, CatalogSnapshot) {
        let dir = tempfile::tempdir().expect("tempdir");
        for (path, content) in files {
            let full = dir.path().join(path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).expect("dirs");
            }
            fs::write(full, content).expect("write");
        }
        let provider = FsCatalogProvider::new(
            RootPolicy::new(vec![dir.path().to_path_buf()]).expect("policy"),
            ScanOptions::default(),
        );
        let snapshot = provider.snapshot().expect("snapshot");
        (dir, provider, snapshot)
    }

    fn request(symbol: &str) -> NavigateRequest {
        NavigateRequest {
            symbol: symbol.to_string(),
            language: None,
            include_definitions: false,
            confident_only: false,
            max_results: 200,
        }
    }

    fn calls(symbol: &str, direction: CallDirection) -> CallHierarchyRequest {
        CallHierarchyRequest {
            symbol: symbol.to_string(),
            direction,
            language: None,
            max_results: 200,
        }
    }

    #[test]
    fn goto_definition_finds_symbol_with_signature_and_snippet() {
        let (_dir, provider, snapshot) = temp_provider(&[(
            "lib.rs",
            "pub struct Widget;\npub fn make_widget() -> Widget { Widget }\n",
        )]);
        let response = goto_definition(&provider, &snapshot, &request("make_widget")).expect("nav");
        assert_eq!(response.total, 1);
        let def = &response.definitions[0];
        assert_eq!(def.path, "lib.rs");
        assert_eq!(def.line, 2);
        assert_eq!(def.kind, "function");
        assert!(
            def.signature
                .as_deref()
                .unwrap_or_default()
                .contains("make_widget")
        );
        assert_eq!(
            def.text.as_deref(),
            Some("pub fn make_widget() -> Widget { Widget }")
        );
    }

    #[test]
    fn find_references_locates_call_sites_with_snippet() {
        let (_dir, provider, snapshot) = temp_provider(&[
            ("target.rs", "pub fn make_target() -> usize { 1 }\n"),
            (
                "caller.rs",
                "pub fn caller() -> usize { make_target() + make_target() }\n",
            ),
        ]);
        let response = find_references(&provider, &snapshot, &request("make_target")).expect("nav");
        assert!(response.total >= 1, "expected at least one reference");
        assert!(response.references.iter().all(|r| r.path == "caller.rs"));
        assert!(
            response.references[0]
                .text
                .as_deref()
                .unwrap_or_default()
                .contains("make_target")
        );
    }

    #[test]
    fn ambiguous_name_marks_low_confidence_and_confident_only_filters() {
        // `helper` defined in two unrelated files; a third file references it
        // without importing either -> ambiguous, low confidence.
        let (_dir, provider, snapshot) = temp_provider(&[
            ("a.rs", "pub fn helper() {}\n"),
            ("b.rs", "pub fn helper() {}\n"),
            ("user.rs", "pub fn run() { helper(); }\n"),
        ]);
        let response = find_references(&provider, &snapshot, &request("helper")).expect("nav");
        assert_eq!(response.definition_count, 2, "two definitions of helper");
        let user_ref = response
            .references
            .iter()
            .find(|r| r.path == "user.rs")
            .expect("user ref");
        assert_eq!(user_ref.confidence, Confidence::Low);

        // confident_only drops the ambiguous user.rs reference.
        let mut req = request("helper");
        req.confident_only = true;
        let filtered = find_references(&provider, &snapshot, &req).expect("nav");
        assert!(!filtered.references.iter().any(|r| r.path == "user.rs"));
    }

    #[test]
    fn unambiguous_name_is_high_confidence() {
        let (_dir, provider, snapshot) = temp_provider(&[
            ("target.rs", "pub fn only_one() {}\n"),
            ("caller.rs", "pub fn run() { only_one(); }\n"),
        ]);
        let response = find_references(&provider, &snapshot, &request("only_one")).expect("nav");
        assert_eq!(response.definition_count, 1);
        assert!(
            response
                .references
                .iter()
                .all(|r| r.confidence == Confidence::High)
        );
    }

    #[test]
    fn find_references_can_include_definitions() {
        let (_dir, provider, snapshot) = temp_provider(&[
            ("target.rs", "pub fn make_target() -> usize { 1 }\n"),
            ("caller.rs", "pub fn caller() { make_target(); }\n"),
        ]);
        let mut req = request("make_target");
        req.include_definitions = true;
        let response = find_references(&provider, &snapshot, &req).expect("nav");
        assert_eq!(response.definitions.len(), 1);
        assert_eq!(response.definitions[0].path, "target.rs");
    }

    #[test]
    fn language_filter_excludes_other_languages() {
        let (_dir, provider, snapshot) = temp_provider(&[
            ("a.rs", "pub fn shared() {}\n"),
            ("b.js", "export function shared() {}\n"),
        ]);
        let mut req = request("shared");
        req.language = Some("rust".to_string());
        let response = goto_definition(&provider, &snapshot, &req).expect("nav");
        assert_eq!(response.total, 1);
        assert_eq!(response.definitions[0].language, "rust");
    }

    #[test]
    fn unknown_symbol_returns_empty() {
        let (_dir, provider, snapshot) = temp_provider(&[("lib.rs", "pub fn alpha() {}\n")]);
        let response = goto_definition(&provider, &snapshot, &request("missing")).expect("nav");
        assert_eq!(response.total, 0);
        assert!(response.definitions.is_empty());
    }

    #[test]
    fn max_results_truncates_and_flags() {
        let (_dir, provider, snapshot) = temp_provider(&[
            ("a.rs", "pub fn dup() {}\n"),
            ("b.rs", "pub fn dup() {}\n"),
            ("c.rs", "pub fn dup() {}\n"),
        ]);
        let mut req = request("dup");
        req.max_results = 2;
        let response = goto_definition(&provider, &snapshot, &req).expect("nav");
        assert_eq!(response.total, 3);
        assert_eq!(response.definitions.len(), 2);
        assert!(response.truncated);
    }

    #[test]
    fn call_hierarchy_incoming_resolves_enclosing_caller() {
        let (_dir, provider, snapshot) = temp_provider(&[
            ("target.rs", "pub fn make_target() -> usize { 1 }\n"),
            (
                "caller.rs",
                "pub fn outer() -> usize {\n    make_target()\n}\n",
            ),
        ]);
        let response = call_hierarchy(
            &provider,
            &snapshot,
            &calls("make_target", CallDirection::Incoming),
        )
        .expect("calls");
        assert!(response.incoming.iter().any(|e| e.symbol == "outer"));
        assert!(response.outgoing.is_empty());
    }

    #[test]
    fn call_hierarchy_outgoing_resolves_callees() {
        let (_dir, provider, snapshot) = temp_provider(&[
            ("helpers.rs", "pub fn helper_a() {}\npub fn helper_b() {}\n"),
            (
                "main.rs",
                "pub fn driver() {\n    helper_a();\n    helper_b();\n}\n",
            ),
        ]);
        let response = call_hierarchy(
            &provider,
            &snapshot,
            &calls("driver", CallDirection::Outgoing),
        )
        .expect("calls");
        let callees: Vec<&str> = response
            .outgoing
            .iter()
            .map(|e| e.symbol.as_str())
            .collect();
        assert!(callees.contains(&"helper_a"));
        assert!(callees.contains(&"helper_b"));
        assert!(response.incoming.is_empty());
    }
}
