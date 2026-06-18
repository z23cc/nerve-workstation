use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Runtime command kinds accepted by the human-facing daemon job protocol.
pub const RUNTIME_COMMAND_NAMES: &[&str] = &["ping", "tool.list", "tool.call", "agent.run"];

/// Transport-neutral command understood by human-facing runtime adapters.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
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
        arguments: BTreeMap<String, Value>,
    },
    /// Run the built-in agent loop as a job. This is protocol vocabulary only:
    /// the host job manager (the composition root) executes it; the core runtime
    /// dispatcher does not (it has no LLM/provider knowledge). Provider/model are
    /// plain data here and translated to domain types by the host.
    #[serde(rename = "agent.run")]
    AgentRun {
        provider: String,
        model: String,
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_turns: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        temperature: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_filter: Option<Vec<String>>,
    },
}

impl RuntimeCommand {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::ToolList => "tool.list",
            Self::ToolCall { .. } => "tool.call",
            Self::AgentRun { .. } => "agent.run",
        }
    }

    #[must_use]
    pub fn tool_name(&self) -> Option<&str> {
        match self {
            Self::ToolCall { name, .. } => Some(name.as_str()),
            Self::Ping | Self::ToolList | Self::AgentRun { .. } => None,
        }
    }
}

fn default_arguments() -> BTreeMap<String, Value> {
    BTreeMap::new()
}
