//! Minimal synchronous MCP client over child-process stdio (JSON-RPC 2.0 / NDJSON).
//!
//! No async runtime: a reader thread pushes the server's stdout lines into a
//! channel, and the (serialized) caller waits on that channel with a timeout,
//! matching responses by id. Callers hold a `Mutex` around the client, so there
//! is only ever one in-flight request per server.

use super::config::McpServerConfig;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

/// MCP protocol version nerve speaks (matches the server side in `server.rs`).
const PROTOCOL_VERSION: &str = "2024-11-05";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct McpStdioClient {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    next_id: u64,
}

impl McpStdioClient {
    /// Spawn the server, perform the MCP initialize handshake, and return a
    /// ready client. Errors if the process cannot start or fails to initialize.
    pub(crate) fn connect(config: &McpServerConfig) -> Result<Self> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (key, value) in &config.env {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to spawn mcp server '{}'", config.name))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("mcp server has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("mcp server has no stdout"))?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            stdin,
            rx,
            next_id: 1,
        };
        client.initialize()?;
        Ok(client)
    }

    fn initialize(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "nerve", "version": env!("CARGO_PKG_VERSION") },
            }),
        )?;
        self.notify("notifications/initialized", json!({}))
    }

    /// List the server's tools (MCP `tools/list`).
    pub(crate) fn list_tools(&mut self) -> Result<Vec<Value>> {
        let result = self.request("tools/list", json!({}))?;
        Ok(result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Call one tool (MCP `tools/call`); returns the raw MCP result object.
    pub(crate) fn call_tool(&mut self, name: &str, arguments: &Value) -> Result<Value> {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let line = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "method": method, "params": params,
        }))?;
        self.write_line(&line)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let line = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params,
        }))?;
        self.write_line(&line)?;
        self.await_response(id)
    }

    fn write_line(&mut self, line: &str) -> Result<()> {
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .context("failed to write to mcp server")
    }

    fn await_response(&mut self, id: u64) -> Result<Value> {
        loop {
            let line = match self.rx.recv_timeout(REQUEST_TIMEOUT) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => bail!("mcp server timed out"),
                Err(RecvTimeoutError::Disconnected) => bail!("mcp server closed the connection"),
            };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            // Skip notifications and responses to other ids.
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                bail!("mcp server error: {error}");
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

impl Drop for McpStdioClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
