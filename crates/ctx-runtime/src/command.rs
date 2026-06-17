use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Runtime command names exposed by the human-facing daemon protocol.
pub const RUNTIME_COMMAND_NAMES: &[&str] = &["ping", "tool.list", "tool.call"];

/// Transport-neutral command understood by human-facing runtime adapters.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind")]
pub enum RuntimeCommand {
    /// Lightweight health check used by clients before opening a real session.
    #[serde(rename = "ping")]
    Ping,
    /// Return all runtime tool specifications.
    #[serde(rename = "tool.list")]
    ToolList,
    /// Execute one MCP-style tool through the runtime dispatcher.
    #[serde(rename = "tool.call")]
    ToolCall {
        name: String,
        #[serde(default = "default_arguments")]
        arguments: Value,
    },
}

impl RuntimeCommand {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::ToolList => "tool.list",
            Self::ToolCall { .. } => "tool.call",
        }
    }

    #[must_use]
    pub fn tool_name(&self) -> Option<&str> {
        match self {
            Self::ToolCall { name, .. } => Some(name.as_str()),
            Self::Ping | Self::ToolList => None,
        }
    }
}

fn default_arguments() -> Value {
    json!({})
}
