//! Provider-neutral chat message and request/response types.
//!
//! These types are the lingua franca between the orchestrator and the
//! individual [`crate::provider::LlmProvider`] implementations. They serialize
//! with `snake_case` tags so they can be embedded in protocol payloads.

use serde::{Deserialize, Serialize};

/// The author role of a [`Message`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A tool the model may call, mirroring an entry of nerve's `tool_specs()`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Tool name (the identifier the model calls).
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema describing the tool's input arguments.
    pub input_schema: serde_json::Value,
}

/// A request from the model to invoke a tool.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned identifier correlating the call to its result.
    pub id: String,
    /// Name of the tool to invoke.
    pub name: String,
    /// Arguments for the tool, as a JSON object.
    pub arguments: serde_json::Value,
}

/// A single conversation message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    /// The author role.
    pub role: Role,
    /// Textual content of the message.
    pub content: String,
    /// Tool calls requested by an assistant message.
    pub tool_calls: Vec<ToolCall>,
    /// For a `Tool` message, the id of the call this result answers.
    pub tool_call_id: Option<String>,
    /// Optional name (e.g. the tool name for a `Tool` message).
    pub name: Option<String>,
}

impl Message {
    /// Build a `System` message.
    pub fn system(content: impl Into<String>) -> Self {
        Self::plain(Role::System, content)
    }

    /// Build a `User` message.
    pub fn user(content: impl Into<String>) -> Self {
        Self::plain(Role::User, content)
    }

    /// Build an `Assistant` message.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::plain(Role::Assistant, content)
    }

    /// Build a `Tool` result message for the call `tool_call_id`.
    pub fn tool(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
            name: Some(name.into()),
        }
    }

    fn plain(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        }
    }
}

/// Why the model stopped generating.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolUse,
    Length,
    Other(String),
}

/// Token accounting for a single response.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Number of input (prompt) tokens.
    pub input_tokens: u32,
    /// Number of output (completion) tokens.
    pub output_tokens: u32,
}

/// An incremental piece of a streaming chat response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ChatDelta {
    /// A chunk of assistant text.
    Text(String),
    /// A chunk of reasoning/thinking text.
    Reasoning(String),
    /// A fully-formed tool call.
    ToolCall(ToolCall),
}

/// A completed (assembled) chat response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatResponse {
    /// Assembled assistant text.
    pub content: String,
    /// Assembled reasoning text, if the model produced any.
    pub reasoning: Option<String>,
    /// Tool calls the model requested.
    pub tool_calls: Vec<ToolCall>,
    /// Why generation stopped.
    pub finish_reason: FinishReason,
    /// Token usage for this response.
    pub usage: Usage,
}

/// A provider-neutral chat completion request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Model identifier.
    pub model: String,
    /// Optional system prompt.
    pub system: Option<String>,
    /// Conversation history.
    pub messages: Vec<Message>,
    /// Tools available to the model.
    pub tools: Vec<ToolSpec>,
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// Maximum tokens to generate.
    pub max_tokens: Option<u32>,
    /// Reasoning effort hint (provider-specific, e.g. "low"/"medium"/"high").
    pub reasoning_effort: Option<String>,
}
