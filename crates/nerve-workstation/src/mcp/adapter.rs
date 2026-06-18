//! MCP-client tool adapter: exposes external MCP servers' tools through the
//! `RuntimeToolAdapter` seam (per the architecture north star — tools-as-plugins,
//! no recompile). Tools are namespaced `mcp__<server>__<tool>` to avoid clashes.

use super::client::McpStdioClient;
use super::config::McpServerConfig;
use nerve_core::WorkspaceRegistry;
use nerve_runtime::{RuntimeError, RuntimeToolAdapter};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex;

pub(crate) struct McpClientToolAdapter {
    /// Connected clients keyed by server name. A per-server `Mutex` serializes
    /// that server's requests while allowing concurrency across servers.
    clients: HashMap<String, Mutex<McpStdioClient>>,
    /// Namespaced tool specs advertised to the runtime.
    specs: Vec<Value>,
}

impl McpClientToolAdapter {
    /// Connect to every configured server, list its tools, and build the
    /// namespaced spec catalog. Servers that fail to connect or list are logged
    /// to stderr and skipped — one bad server must not break the runtime.
    pub(crate) fn connect(configs: Vec<McpServerConfig>) -> Self {
        let mut clients = HashMap::new();
        let mut specs = Vec::new();
        for config in configs {
            match McpStdioClient::connect(&config) {
                Ok(mut client) => match client.list_tools() {
                    Ok(tools) => {
                        for tool in &tools {
                            if let Some(spec) = namespaced_spec(&config.name, tool) {
                                specs.push(spec);
                            }
                        }
                        clients.insert(config.name.clone(), Mutex::new(client));
                    }
                    Err(err) => eprintln!("[mcp] '{}' tools/list failed: {err}", config.name),
                },
                Err(err) => eprintln!("[mcp] '{}' connect failed: {err}", config.name),
            }
        }
        Self { clients, specs }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }
}

impl RuntimeToolAdapter<WorkspaceRegistry> for McpClientToolAdapter {
    fn tool_specs(&self) -> Vec<Value> {
        self.specs.clone()
    }

    fn handle_tool_call(
        &self,
        _resolver: &WorkspaceRegistry,
        params: &Value,
    ) -> Result<Option<Value>, RuntimeError> {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return Ok(None);
        };
        let Some((server, tool)) = parse_namespaced(name) else {
            return Ok(None);
        };
        let Some(client) = self.clients.get(server) else {
            return Ok(None);
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mut client = client
            .lock()
            .map_err(|_| RuntimeError::adapter("mcp client lock poisoned"))?;
        let result = client
            .call_tool(tool, &arguments)
            .map_err(|err| RuntimeError::adapter(err.to_string()))?;
        Ok(Some(result))
    }
}

/// Rewrite an MCP tool spec's name to `mcp__<server>__<tool>`, keeping its
/// description and input schema intact.
fn namespaced_spec(server: &str, tool: &Value) -> Option<Value> {
    let name = tool.get("name").and_then(Value::as_str)?;
    let mut spec = tool.clone();
    spec.as_object_mut()?
        .insert("name".to_string(), json!(format!("mcp__{server}__{name}")));
    Some(spec)
}

/// Parse `mcp__<server>__<tool>` back into `(server, tool)`. Server names never
/// contain `__` (enforced at config load), so the first `__` split is correct.
fn parse_namespaced(name: &str) -> Option<(&str, &str)> {
    name.strip_prefix("mcp__")?.split_once("__")
}

#[cfg(test)]
mod tests {
    use super::{namespaced_spec, parse_namespaced};
    use serde_json::json;

    #[test]
    fn namespaces_tool_name_and_keeps_schema() {
        let spec = namespaced_spec(
            "fs",
            &json!({ "name": "read_file", "description": "d", "inputSchema": { "type": "object" } }),
        )
        .expect("spec");
        assert_eq!(spec["name"], "mcp__fs__read_file");
        assert_eq!(spec["description"], "d");
        assert_eq!(spec["inputSchema"]["type"], "object");
    }

    #[test]
    fn parses_namespaced_names() {
        assert_eq!(
            parse_namespaced("mcp__fs__read_file"),
            Some(("fs", "read_file"))
        );
        // A tool whose own name contains `__` round-trips (server has no `__`).
        assert_eq!(
            parse_namespaced("mcp__git__log__oneline"),
            Some(("git", "log__oneline"))
        );
        assert_eq!(parse_namespaced("file_search"), None);
        assert_eq!(parse_namespaced("mcp__lonely"), None);
    }
}
