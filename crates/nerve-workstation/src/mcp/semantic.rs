//! Non-deterministic semantic / embedding search — CONSUMED above the kernel.
//!
//! `nerve-core` is a fully-deterministic kernel: the ONNX embedding stack was
//! removed in `4e2aa58` precisely because embeddings are model/float/version
//! dependent and network-downloading — the one thing that broke kernel purity
//! (INV-R2). Concept search re-enters HERE, in the impure workstation layer,
//! behind the declared [`RuntimeToolAdapter`] seam, as a CONSUMED capability
//! tagged `deterministic: false` (the build-vs-consume line *is* the determinism
//! boundary — see `docs/designs/code-graph.md`).
//!
//! ## The invariant this preserves (INV-R1)
//! The deterministic `scout` / `build_context` ranking lives entirely in
//! `nerve-core` (BM25 + repo-map PageRank + path, computed from the snapshot).
//! This tool is a SEPARATE workstation adapter — a different crate that the
//! kernel never calls — so it *physically cannot* feed that ranking. A captured
//! Run's context stays bit-for-bit replayable while humans / scout agents get
//! fuzzy concept recall as an **advisory overlay** they then cite with the
//! deterministic tools.
//!
//! ## Backend (consume, never build)
//! The shipped backend consumes an external embedding MCP server over the stdio
//! client. The server's designated tool must return
//! `structuredContent.hits = [{ path, score?, ranges?, note? }]`. The
//! [`SemanticBackend`] trait keeps the consumer testable with a deterministic
//! fake and lets a future in-process backend slot in without touching callers.

use super::client::McpStdioClient;
use super::config::{McpServerConfig, SemanticBackendConfig};
use nerve_fs::FsWorkspaceRegistry;
use nerve_runtime::{RiskTier, RuntimeError, RuntimeToolAdapter, ToolCapability};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Mutex;

const TOOL_NAME: &str = "semantic_search";
const DEFAULT_MAX_RESULTS: usize = 12;
const MAX_RESULTS_CAP: usize = 100;

/// One semantic hit, shaped to mirror a `scout` citation so a client renders it
/// the same way — but produced by a non-deterministic backend.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SemanticHit {
    pub(crate) path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) score: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) ranges: Vec<SemanticRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
}

/// A 1-based, inclusive line range (mirrors `scout`'s `ScoutRange`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct SemanticRange {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// A pluggable concept-search backend. Implementations are NON-deterministic by
/// contract (embeddings / external models); the adapter labels every result
/// accordingly and never lets it reach the deterministic kernel.
pub(crate) trait SemanticBackend: Send + Sync {
    /// Return up to `max_results` hits for `query`, or a human-readable error.
    fn search(&self, query: &str, max_results: usize) -> Result<Vec<SemanticHit>, String>;
    /// Short provenance label of this backend (surfaced in the result).
    fn label(&self) -> &str;
}

/// The shipped backend: consume an external embedding MCP server. Holds its own
/// stdio client (separate from the generic [`super::adapter::McpClientToolAdapter`]
/// connections) and calls one designated tool, expecting the documented hit
/// contract on `structuredContent.hits`.
pub(crate) struct McpSemanticBackend {
    client: Mutex<McpStdioClient>,
    tool: String,
    label: String,
}

impl McpSemanticBackend {
    /// Spawn + initialize the embedding MCP server from `config`.
    pub(crate) fn connect(config: &SemanticBackendConfig) -> anyhow::Result<Self> {
        let server = McpServerConfig {
            name: "semantic".to_string(),
            command: config.command.clone(),
            args: config.args.clone(),
            env: config.env.clone(),
        };
        let client = McpStdioClient::connect(&server)?;
        Ok(Self {
            client: Mutex::new(client),
            label: format!("mcp:{}/{}", config.command, config.tool),
            tool: config.tool.clone(),
        })
    }
}

impl SemanticBackend for McpSemanticBackend {
    fn search(&self, query: &str, max_results: usize) -> Result<Vec<SemanticHit>, String> {
        let args = json!({ "query": query, "max_results": max_results });
        let result = {
            let mut client = crate::sync::lock_recover(&self.client);
            client
                .call_tool(&self.tool, &args)
                .map_err(|e| e.to_string())?
        };
        parse_hits(&result)
    }

    fn label(&self) -> &str {
        &self.label
    }
}

/// Parse the backend MCP tool result into hits: read `structuredContent.hits`,
/// else an empty set — honest (no fabricated hits) rather than guessing a shape.
fn parse_hits(result: &Value) -> Result<Vec<SemanticHit>, String> {
    let hits = result
        .get("structuredContent")
        .and_then(|s| s.get("hits"))
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    serde_json::from_value(hits)
        .map_err(|e| format!("semantic backend returned malformed hits: {e}"))
}

/// The `semantic_search` tool adapter — NON-deterministic, advisory, and isolated
/// from the captured deterministic context by construction (it lives above the
/// kernel and never feeds `build_context`).
pub(crate) struct SemanticSearchAdapter {
    backend: Box<dyn SemanticBackend>,
}

impl SemanticSearchAdapter {
    pub(crate) fn new(backend: Box<dyn SemanticBackend>) -> Self {
        Self { backend }
    }

    fn spec() -> Value {
        json!({
            "name": TOOL_NAME,
            "description": "NON-DETERMINISTIC concept/semantic search over the workspace via an \
                external embedding backend. Use it to find where code about a CONCEPT lives when \
                you don't know the exact identifiers — it bridges the vocabulary gap that lexical \
                `file_search`/`scout` cannot (e.g. query \"retry handling\" matching code that \
                says `backoff`/`attempt`). ADVISORY ONLY: results are NOT part of the deterministic, \
                replayable captured context — use them to LOCATE, then cite/read with the \
                deterministic tools. Returns { deterministic: false, hits: [{ path, ranges, score, \
                note }] }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural-language concept to search for."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum hits to return (default 12, capped at 100)."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        })
    }

    fn run(&self, params: &Value) -> Result<Value, RuntimeError> {
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if query.is_empty() {
            return Err(RuntimeError::adapter(
                "semantic_search requires a non-empty `query`",
            ));
        }
        let max_results = args
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_RESULTS)
            .clamp(1, MAX_RESULTS_CAP);
        let hits = self
            .backend
            .search(query, max_results)
            .map_err(RuntimeError::adapter)?;
        Ok(render(query, self.backend.label(), &hits))
    }
}

/// Render hits as an MCP tool result: a human text block PLUS `structuredContent`
/// carrying the explicit `deterministic: false` marker so any consumer (and the
/// recorded tape) sees the provenance in-band.
fn render(query: &str, backend_label: &str, hits: &[SemanticHit]) -> Value {
    let mut lines = Vec::with_capacity(hits.len());
    for hit in hits {
        let ranges = if hit.ranges.is_empty() {
            String::new()
        } else {
            let parts: Vec<String> = hit
                .ranges
                .iter()
                .map(|r| format!("{}-{}", r.start, r.end))
                .collect();
            format!(":{}", parts.join(","))
        };
        let score = hit.score.map(|s| format!(" — {s:.4}")).unwrap_or_default();
        let note = hit
            .note
            .as_deref()
            .map(|n| format!("  {n}"))
            .unwrap_or_default();
        lines.push(format!("  {}{}{}{}", hit.path, ranges, score, note));
    }
    let text = format!(
        "semantic_search [deterministic:false, backend={backend_label}]: {} hit(s) for \"{query}\"\n{}\n(advisory — NOT part of the captured deterministic context; cite with the deterministic tools)",
        hits.len(),
        lines.join("\n")
    );
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": {
            "deterministic": false,
            "backend": backend_label,
            "query": query,
            "hits": hits,
        }
    })
}

impl RuntimeToolAdapter<FsWorkspaceRegistry> for SemanticSearchAdapter {
    fn tool_specs(&self) -> Vec<Value> {
        vec![Self::spec()]
    }

    fn handle_tool_call(
        &self,
        _registry: &FsWorkspaceRegistry,
        params: &Value,
    ) -> Result<Option<Value>, RuntimeError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if name != TOOL_NAME {
            return Ok(None);
        }
        self.run(params).map(Some)
    }

    fn tool_capability(&self, _name: &str) -> ToolCapability {
        // The whole point of this tool: an honest NON-deterministic descriptor.
        ToolCapability {
            risk: RiskTier::ReadOnly,
            reads_fs: true,
            writes_fs: false,
            network: true,
            deterministic: false,
        }
    }

    fn owns(&self, name: &str) -> bool {
        name == TOOL_NAME
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nerve_runtime::Runtime;

    /// A deterministic fake backend (canned hits) — proves the adapter wiring and
    /// the `deterministic: false` tagging without a real embedding model/network.
    struct FakeBackend;

    impl SemanticBackend for FakeBackend {
        fn search(&self, _query: &str, _max_results: usize) -> Result<Vec<SemanticHit>, String> {
            Ok(vec![SemanticHit {
                path: "/repo/src/retry.rs".to_string(),
                score: Some(0.91),
                ranges: vec![SemanticRange { start: 10, end: 24 }],
                note: Some("backoff loop".to_string()),
            }])
        }
        fn label(&self) -> &str {
            "fake"
        }
    }

    fn adapter() -> SemanticSearchAdapter {
        SemanticSearchAdapter::new(Box::new(FakeBackend))
    }

    fn call(name: &str, args: Value) -> Result<Option<Value>, RuntimeError> {
        let registry = FsWorkspaceRegistry::new();
        adapter().handle_tool_call(&registry, &json!({ "name": name, "arguments": args }))
    }

    #[test]
    fn result_is_tagged_non_deterministic_with_hits() {
        let out = call(TOOL_NAME, json!({ "query": "retry handling" }))
            .expect("ok")
            .expect("owned tool returns Some");
        let structured = &out["structuredContent"];
        assert_eq!(structured["deterministic"], json!(false));
        assert_eq!(structured["backend"], json!("fake"));
        assert_eq!(structured["hits"].as_array().expect("hits").len(), 1);
        assert_eq!(structured["hits"][0]["path"], json!("/repo/src/retry.rs"));
        // The human text block also discloses the non-determinism in-band.
        let text = out["content"][0]["text"].as_str().expect("text");
        assert!(text.contains("deterministic:false"), "{text}");
        assert!(text.contains("advisory"), "{text}");
    }

    #[test]
    fn adapter_only_owns_semantic_search() {
        let a = adapter();
        assert!(a.owns(TOOL_NAME));
        assert!(!a.owns("file_search"));
        assert!(!a.owns("build_context"));
    }

    #[test]
    fn unowned_tool_is_passed_through() {
        assert!(
            call("file_search", json!({ "query": "x" }))
                .expect("ok")
                .is_none(),
            "a non-owned tool must return None so dispatch continues"
        );
    }

    #[test]
    fn capability_is_readonly_and_non_deterministic() {
        let cap = adapter().tool_capability(TOOL_NAME);
        assert_eq!(cap.risk, RiskTier::ReadOnly);
        assert!(
            !cap.deterministic,
            "semantic search must declare non-determinism"
        );
        assert!(!cap.writes_fs);
        assert!(cap.network);
    }

    #[test]
    fn empty_query_is_rejected() {
        let err = call(TOOL_NAME, json!({ "query": "  " })).expect_err("empty query rejected");
        assert!(err.to_string().contains("non-empty"), "{err}");
    }

    #[test]
    fn not_present_in_the_deterministic_kernel() {
        // Isolation: the bare kernel runtime (no semantic adapter) does NOT know
        // `semantic_search` — proving it lives strictly above the determinism
        // boundary and can never enter a captured Run's deterministic context.
        let registry = FsWorkspaceRegistry::new();
        let runtime = Runtime::new(registry);
        let specs = runtime.tool_specs();
        let names: Vec<&str> = specs
            .as_array()
            .expect("specs")
            .iter()
            .filter_map(|s| s.get("name").and_then(Value::as_str))
            .collect();
        assert!(
            !names.contains(&TOOL_NAME),
            "semantic_search must NOT be a kernel tool: {names:?}"
        );
        assert!(
            runtime
                .handle_tool_call(&json!({ "name": TOOL_NAME, "arguments": { "query": "x" } }))
                .is_err(),
            "kernel dispatch must not resolve semantic_search"
        );
    }
}
