//! Workstation ToolBox adapters for `nerve-agent`.
//!
//! These adapters live in the binary composition root: `nerve-agent` only sees
//! the generic [`ToolBox`] seam, while tool execution still flows through the
//! runtime dispatch hub.

use crate::tools::NerveRuntime;
use nerve_agent::{AgentError, AgentResult, ToolBox, ToolSpec};
use nerve_core::CancelToken;
use serde_json::{Value, json};
use std::sync::Arc;

/// Upper bound on a single tool result fed back to the model. nerve tools can
/// return very large payloads (whole-file reads, repo maps); capping the first
/// appearance keeps one call from dominating the context window. The
/// orchestrator additionally elides older tool outputs as history grows.
const MAX_TOOL_OUTPUT_CHARS: usize = 24_000;

/// Bridges nerve's tool [`Runtime`](NerveRuntime) to the agent's [`ToolBox`]
/// seam: tool specs are read from the runtime and calls are dispatched through
/// the same path the MCP/daemon adapters use.
pub(crate) struct RuntimeToolBox {
    runtime: Arc<NerveRuntime>,
}

impl RuntimeToolBox {
    pub(crate) fn new(runtime: Arc<NerveRuntime>) -> Self {
        Self { runtime }
    }
}

impl ToolBox for RuntimeToolBox {
    fn specs(&self) -> Vec<ToolSpec> {
        let specs = self.runtime.tool_specs();
        specs
            .as_array()
            .map(|tools| tools.iter().filter_map(parse_tool_spec).collect())
            .unwrap_or_default()
    }

    fn call(&self, name: &str, args: &Value, cancel: &CancelToken) -> AgentResult<Value> {
        let params = json!({ "name": name, "arguments": args });
        let result = self
            .runtime
            .handle_tool_call_cancellable(&params, cancel)
            .map_err(|err| AgentError::Tool(err.to_string()))?;
        let value = result.get("structuredContent").cloned().unwrap_or(result);
        Ok(cap_tool_output(value))
    }
}

fn parse_tool_spec(tool: &Value) -> Option<ToolSpec> {
    let name = tool.get("name")?.as_str()?.to_string();
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let input_schema = tool
        .get("inputSchema")
        .cloned()
        .unwrap_or_else(|| json!({ "type": "object" }));
    Some(ToolSpec {
        name,
        description,
        input_schema,
    })
}

/// Cap a tool result so a single call cannot dominate the context window. Small
/// results pass through unchanged (preserving structure); oversized ones are
/// rendered to text, truncated, and tagged so the model knows the view is
/// partial.
fn cap_tool_output(value: Value) -> Value {
    let text = match &value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let total = text.chars().count();
    if total <= MAX_TOOL_OUTPUT_CHARS {
        return value;
    }
    let head: String = text.chars().take(MAX_TOOL_OUTPUT_CHARS).collect();
    Value::String(format!(
        "{head}\n…[tool output truncated: {MAX_TOOL_OUTPUT_CHARS} of {total} characters shown]"
    ))
}
