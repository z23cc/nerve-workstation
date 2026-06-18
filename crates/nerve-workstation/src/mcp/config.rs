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

#[derive(Debug, Deserialize)]
struct McpServersFile {
    #[serde(default)]
    servers: Vec<McpServerConfig>,
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
    }
}
