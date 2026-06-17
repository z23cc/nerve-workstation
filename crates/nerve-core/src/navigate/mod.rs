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
    models::NerveError,
    port::CatalogProvider,
    repomap::{IndexedFile, indexed_files_cancellable, resolve_import_reference},
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
fn sort_locations(locations: &mut [SymbolLocation]) {
    locations.sort_by(|a, b| {
        a.display_path
            .cmp(&b.display_path)
            .then(a.line.cmp(&b.line))
            .then(a.kind.cmp(&b.kind))
    });
}
mod call_hierarchy;
mod definition;
mod references;
mod source;

pub use call_hierarchy::{call_hierarchy, call_hierarchy_cancellable};
pub use definition::{goto_definition, goto_definition_cancellable};
pub use references::{find_references, find_references_cancellable};

use source::*;

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
