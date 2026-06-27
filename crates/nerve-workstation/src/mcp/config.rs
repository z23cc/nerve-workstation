//! MCP server configuration — which external MCP servers to expose as tools.

use crate::workspace::ServeArgs;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

/// One external MCP server, launched as a child process spoken to over stdio.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct McpServerConfig {
    /// Short name; namespaces the server's tools as `mcp__<name>__<tool>`.
    /// Must not contain `__`.
    pub(crate) name: String,
    /// Executable to spawn.
    pub(crate) command: String,
    /// Arguments passed to the executable.
    #[serde(default)]
    pub(crate) args: Vec<String>,
    /// Extra environment variables for the child process.
    #[serde(default)]
    pub(crate) env: BTreeMap<String, String>,
}

/// The optional non-deterministic semantic-search backend: a dedicated embedding
/// MCP server nerve spawns and queries for concept recall. Distinct from the
/// generic `servers` list — it is consumed by [`crate::mcp::semantic`] and
/// surfaced as the single `semantic_search` tool (tagged `deterministic: false`),
/// never namespaced as `mcp__*`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SemanticBackendConfig {
    /// Executable to spawn as the embedding backend.
    pub(crate) command: String,
    /// Arguments passed to the executable.
    #[serde(default)]
    pub(crate) args: Vec<String>,
    /// Extra environment variables for the child process.
    #[serde(default)]
    pub(crate) env: BTreeMap<String, String>,
    /// The tool on that server to call; it must return
    /// `structuredContent.hits = [{ path, score?, ranges?, note? }]`.
    #[serde(default = "default_semantic_tool")]
    pub(crate) tool: String,
}

fn default_semantic_tool() -> String {
    "semantic_search".to_string()
}

#[derive(Debug, Deserialize)]
struct McpServersFile {
    #[serde(default)]
    servers: Vec<McpServerConfig>,
    #[serde(default)]
    semantic: Option<SemanticBackendConfig>,
}

/// Load the MCP server list from the `--mcp-config` JSON file, if provided.
/// Returns an empty list when no config path is set.
pub(crate) fn load_from_args(args: &ServeArgs) -> Result<Vec<McpServerConfig>> {
    let Some(path) = args.mcp_config.as_ref() else {
        return Ok(Vec::new());
    };
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read mcp config: {}", path.display()))?;
    let parsed: McpServersFile = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse mcp config: {}", path.display()))?;
    Ok(parsed
        .servers
        .into_iter()
        .filter(|server| !server.name.contains("__"))
        .collect())
}

/// Load the optional `semantic` backend from the `--mcp-config` JSON file, if any.
/// `None` when no config path is set or the file omits the `semantic` key.
pub(crate) fn load_semantic_from_args(args: &ServeArgs) -> Result<Option<SemanticBackendConfig>> {
    let Some(path) = args.mcp_config.as_ref() else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read mcp config: {}", path.display()))?;
    let parsed: McpServersFile = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse mcp config: {}", path.display()))?;
    Ok(parsed.semantic)
}

#[cfg(test)]
mod tests {
    use super::McpServersFile;

    #[test]
    fn parses_servers() {
        let file: McpServersFile = serde_json::from_str(
            r#"{ "servers": [ { "name": "fs", "command": "mcp-fs", "args": ["--root", "/x"] } ] }"#,
        )
        .expect("parse");
        assert_eq!(file.servers.len(), 1);
        assert_eq!(file.servers[0].name, "fs");
        assert_eq!(file.servers[0].command, "mcp-fs");
        assert_eq!(file.servers[0].args, vec!["--root", "/x"]);
        assert!(file.servers[0].env.is_empty());
    }

    #[test]
    fn empty_when_no_servers_key() {
        let file: McpServersFile = serde_json::from_str("{}").expect("parse");
        assert!(file.servers.is_empty());
        assert!(file.semantic.is_none());
    }

    #[test]
    fn parses_semantic_backend_with_default_tool() {
        let file: McpServersFile = serde_json::from_str(
            r#"{ "semantic": { "command": "embed-mcp", "args": ["--model", "code"] } }"#,
        )
        .expect("parse");
        let sem = file.semantic.expect("semantic backend");
        assert_eq!(sem.command, "embed-mcp");
        assert_eq!(sem.args, vec!["--model", "code"]);
        assert_eq!(sem.tool, "semantic_search", "tool defaults when omitted");
    }

    #[test]
    fn semantic_tool_name_is_overridable() {
        let file: McpServersFile =
            serde_json::from_str(r#"{ "semantic": { "command": "e", "tool": "concept_find" } }"#)
                .expect("parse");
        assert_eq!(file.semantic.expect("semantic").tool, "concept_find");
    }
}
