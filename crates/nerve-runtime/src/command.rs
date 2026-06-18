use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Runtime command kinds accepted by the human-facing daemon job protocol.
pub const RUNTIME_COMMAND_NAMES: &[&str] = &[
    "ping",
    "tool.list",
    "tool.call",
    "agent.run",
    "session.start",
    "session.message",
    "session.interrupt",
    "session.respond",
    "session.get",
    "session.list",
    "session.close",
    "auth.start",
    "auth.complete",
    "auth.status",
    "auth.logout",
];

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
    /// Start or resume a host-managed interactive agent session.
    #[serde(rename = "session.start")]
    SessionStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        provider: String,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_turns: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        temperature: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_filter: Option<Vec<String>>,
    },
    /// Send a user message to an existing host-managed session.
    #[serde(rename = "session.message")]
    SessionMessage { session_id: String, text: String },
    /// Interrupt the current turn of an existing host-managed session.
    #[serde(rename = "session.interrupt")]
    SessionInterrupt { session_id: String },
    /// Reply to a session approval request.
    #[serde(rename = "session.respond")]
    SessionRespond {
        session_id: String,
        request_id: String,
        decision: SessionApprovalDecision,
    },
    /// Fetch one host-managed session.
    #[serde(rename = "session.get")]
    SessionGet { session_id: String },
    /// List host-managed sessions.
    #[serde(rename = "session.list")]
    SessionList,
    /// Close a host-managed session.
    #[serde(rename = "session.close")]
    SessionClose { session_id: String },
    /// Start a host-managed OAuth login and return an authorization URL.
    #[serde(rename = "auth.start")]
    AuthStart { provider: String },
    /// Complete a host-managed OAuth login with a code or pasted callback URL.
    #[serde(rename = "auth.complete")]
    AuthComplete {
        login_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        callback_url: Option<String>,
    },
    /// Return stored OAuth/API-key credential status without secrets.
    #[serde(rename = "auth.status")]
    AuthStatus { provider: String },
    /// Remove stored credentials for a provider.
    #[serde(rename = "auth.logout")]
    AuthLogout { provider: String },
}

/// Decision supplied by a human/client for a session approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionApprovalDecision {
    Allow,
    Deny,
}

impl RuntimeCommand {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::ToolList => "tool.list",
            Self::ToolCall { .. } => "tool.call",
            Self::AgentRun { .. } => "agent.run",
            Self::SessionStart { .. } => "session.start",
            Self::SessionMessage { .. } => "session.message",
            Self::SessionInterrupt { .. } => "session.interrupt",
            Self::SessionRespond { .. } => "session.respond",
            Self::SessionGet { .. } => "session.get",
            Self::SessionList => "session.list",
            Self::SessionClose { .. } => "session.close",
            Self::AuthStart { .. } => "auth.start",
            Self::AuthComplete { .. } => "auth.complete",
            Self::AuthStatus { .. } => "auth.status",
            Self::AuthLogout { .. } => "auth.logout",
        }
    }

    #[must_use]
    pub fn tool_name(&self) -> Option<&str> {
        match self {
            Self::ToolCall { name, .. } => Some(name.as_str()),
            Self::Ping
            | Self::ToolList
            | Self::AgentRun { .. }
            | Self::SessionStart { .. }
            | Self::SessionMessage { .. }
            | Self::SessionInterrupt { .. }
            | Self::SessionRespond { .. }
            | Self::SessionGet { .. }
            | Self::SessionList
            | Self::SessionClose { .. }
            | Self::AuthStart { .. }
            | Self::AuthComplete { .. }
            | Self::AuthStatus { .. }
            | Self::AuthLogout { .. } => None,
        }
    }
}

fn default_arguments() -> BTreeMap<String, Value> {
    BTreeMap::new()
}
