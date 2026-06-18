//! Anthropic Messages SSE parsing: a small state machine that folds the event
//! stream into streamed [`ChatDelta`]s and a final [`ChatResponse`].
//!
//! Mirrors `_parse_claude_sse` in `GenericAgent/llmcore.py`: it handles
//! `message_start`, `content_block_start`/`_delta`/`_stop`, `message_delta`,
//! `message_stop`, `error`, and trailing `usage`.

use serde_json::Value;

use crate::error::{AgentError, AgentResult};
use crate::message::{ChatDelta, ChatResponse, FinishReason, ToolCall, Usage};

/// The currently-open content block while streaming.
enum OpenBlock {
    /// A `text` block (accumulated into the assistant content).
    Text,
    /// A `thinking` block (accumulated into the reasoning content).
    Thinking,
    /// A `tool_use` block; its arguments arrive as partial JSON fragments.
    ToolUse {
        id: String,
        name: String,
        json_buf: String,
    },
}

/// Folds Anthropic SSE events into the assembled [`ChatResponse`].
#[derive(Default)]
pub struct ResponseBuilder {
    content: String,
    reasoning: String,
    tool_calls: Vec<ToolCall>,
    finish_reason: Option<FinishReason>,
    usage: Usage,
    current: Option<OpenBlock>,
}

impl ResponseBuilder {
    /// Create an empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one decoded SSE event, emitting any text/reasoning/tool deltas
    /// into `sink`. Returns `true` once a terminal `message_stop` is seen.
    pub fn apply(&mut self, evt: &Value, sink: &mut dyn FnMut(ChatDelta)) -> AgentResult<bool> {
        match evt.get("type").and_then(Value::as_str).unwrap_or_default() {
            "message_start" => {
                if let Some(usage) = evt.pointer("/message/usage") {
                    self.merge_usage(usage);
                }
                Ok(false)
            }
            "content_block_start" => {
                self.open_block(evt.get("content_block"));
                Ok(false)
            }
            "content_block_delta" => {
                self.apply_delta(evt.get("delta"), sink);
                Ok(false)
            }
            "content_block_stop" => {
                self.finish_block(sink);
                Ok(false)
            }
            "message_delta" => {
                self.apply_message_delta(evt);
                Ok(false)
            }
            "message_stop" => Ok(true),
            "error" => Err(error_from_event(evt)),
            _ => Ok(false),
        }
    }

    /// Open a new content block from a `content_block_start` payload.
    fn open_block(&mut self, block: Option<&Value>) {
        let Some(block) = block else { return };
        self.current = match block.get("type").and_then(Value::as_str) {
            Some("text") => Some(OpenBlock::Text),
            Some("thinking") => Some(OpenBlock::Thinking),
            Some("tool_use") => Some(OpenBlock::ToolUse {
                id: str_field(block, "id"),
                name: str_field(block, "name"),
                json_buf: String::new(),
            }),
            _ => None,
        };
    }

    /// Apply a `content_block_delta`, streaming text/reasoning or buffering
    /// tool-call argument JSON.
    fn apply_delta(&mut self, delta: Option<&Value>, sink: &mut dyn FnMut(ChatDelta)) {
        let Some(delta) = delta else { return };
        match delta.get("type").and_then(Value::as_str) {
            Some("text_delta") => {
                let text = str_field(delta, "text");
                if !text.is_empty() {
                    self.content.push_str(&text);
                    sink(ChatDelta::Text(text));
                }
            }
            Some("thinking_delta") => {
                let text = str_field(delta, "thinking");
                if !text.is_empty() {
                    self.reasoning.push_str(&text);
                    sink(ChatDelta::Reasoning(text));
                }
            }
            Some("input_json_delta") => {
                if let Some(OpenBlock::ToolUse { json_buf, .. }) = self.current.as_mut() {
                    json_buf.push_str(&str_field(delta, "partial_json"));
                }
            }
            _ => {}
        }
    }

    /// Finalize the open block. A completed `tool_use` block is parsed and both
    /// pushed to `tool_calls` and emitted as a [`ChatDelta::ToolCall`].
    fn finish_block(&mut self, sink: &mut dyn FnMut(ChatDelta)) {
        let Some(OpenBlock::ToolUse { id, name, json_buf }) = self.current.take() else {
            return;
        };
        let arguments = parse_tool_args(&json_buf);
        let call = ToolCall {
            id,
            name,
            arguments,
        };
        self.tool_calls.push(call.clone());
        sink(ChatDelta::ToolCall(call));
    }

    /// Apply a `message_delta`: capture the stop reason and any output usage.
    fn apply_message_delta(&mut self, evt: &Value) {
        if let Some(reason) = evt.pointer("/delta/stop_reason").and_then(Value::as_str) {
            self.finish_reason = Some(map_finish_reason(reason));
        }
        if let Some(usage) = evt.get("usage") {
            self.merge_usage(usage);
        }
    }

    /// Merge token counts from a `usage` object, keeping the larger of any
    /// previously-seen value (Anthropic reports input once, output cumulatively).
    fn merge_usage(&mut self, usage: &Value) {
        if let Some(input) = usage.get("input_tokens").and_then(Value::as_u64) {
            self.usage.input_tokens = self.usage.input_tokens.max(input as u32);
        }
        if let Some(output) = usage.get("output_tokens").and_then(Value::as_u64) {
            self.usage.output_tokens = self.usage.output_tokens.max(output as u32);
        }
    }

    /// Consume the builder and assemble the final [`ChatResponse`].
    ///
    /// If no explicit stop reason was seen but tool calls were produced, the
    /// reason defaults to [`FinishReason::ToolUse`]; otherwise [`FinishReason::Stop`].
    pub fn finish(self) -> ChatResponse {
        let finish_reason = self.finish_reason.unwrap_or({
            if self.tool_calls.is_empty() {
                FinishReason::Stop
            } else {
                FinishReason::ToolUse
            }
        });
        let reasoning = if self.reasoning.is_empty() {
            None
        } else {
            Some(self.reasoning)
        };
        ChatResponse {
            content: self.content,
            reasoning,
            tool_calls: self.tool_calls,
            finish_reason,
            usage: self.usage,
        }
    }
}

/// Map an Anthropic `stop_reason` onto a neutral [`FinishReason`].
fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "tool_use" => FinishReason::ToolUse,
        "max_tokens" => FinishReason::Length,
        other => FinishReason::Other(other.to_string()),
    }
}

/// Parse buffered tool-call argument JSON, defaulting to an empty object when
/// the buffer is empty and to a `{"_raw": …}` wrapper when it is malformed.
fn parse_tool_args(buf: &str) -> Value {
    if buf.trim().is_empty() {
        return Value::Object(serde_json::Map::new());
    }
    serde_json::from_str(buf).unwrap_or_else(|_| serde_json::json!({ "_raw": buf }))
}

/// Turn an `error` event into a provider error.
fn error_from_event(evt: &Value) -> AgentError {
    let msg = evt
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            evt.get("error")
                .map(ToString::to_string)
                .unwrap_or_default()
        });
    AgentError::Provider(format!("anthropic stream error: {msg}"))
}

/// Read a string field, defaulting to empty.
fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Drive the builder through `events`, collecting deltas, and assemble.
    fn run(events: &[Value]) -> (Vec<ChatDelta>, ChatResponse, bool) {
        let mut builder = ResponseBuilder::new();
        let mut deltas = Vec::new();
        let mut stopped = false;
        for evt in events {
            let mut sink = |d: ChatDelta| deltas.push(d);
            if builder.apply(evt, &mut sink).unwrap() {
                stopped = true;
            }
        }
        let resp = builder.finish();
        (deltas, resp, stopped)
    }

    #[test]
    fn assembles_text_and_usage() {
        let events = vec![
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 10}}}),
            json!({"type": "content_block_start", "content_block": {"type": "text"}}),
            json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "Hel"}}),
            json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "lo"}}),
            json!({"type": "content_block_stop"}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
            json!({"type": "message_stop"}),
        ];
        let (deltas, resp, stopped) = run(&events);
        assert!(stopped);
        assert_eq!(resp.content, "Hello");
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        assert_eq!(
            resp.usage,
            Usage {
                input_tokens: 10,
                output_tokens: 3
            }
        );
        assert!(matches!(deltas[0], ChatDelta::Text(ref s) if s == "Hel"));
        assert!(matches!(deltas[1], ChatDelta::Text(ref s) if s == "lo"));
    }

    #[test]
    fn assembles_thinking_into_reasoning() {
        let events = vec![
            json!({"type": "content_block_start", "content_block": {"type": "thinking"}}),
            json!({"type": "content_block_delta", "delta": {"type": "thinking_delta", "thinking": "hmm"}}),
            json!({"type": "content_block_stop"}),
            json!({"type": "message_stop"}),
        ];
        let (deltas, resp, _) = run(&events);
        assert_eq!(resp.reasoning.as_deref(), Some("hmm"));
        assert!(matches!(deltas[0], ChatDelta::Reasoning(ref s) if s == "hmm"));
    }

    #[test]
    fn assembles_tool_use_from_input_json_delta() {
        let events = vec![
            json!({"type": "content_block_start", "content_block": {"type": "tool_use", "id": "tu_1", "name": "search"}}),
            json!({"type": "content_block_delta", "delta": {"type": "input_json_delta", "partial_json": "{\"q\":"}}),
            json!({"type": "content_block_delta", "delta": {"type": "input_json_delta", "partial_json": "\"rust\"}"}}),
            json!({"type": "content_block_stop"}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}),
            json!({"type": "message_stop"}),
        ];
        let (deltas, resp, _) = run(&events);
        assert_eq!(resp.finish_reason, FinishReason::ToolUse);
        assert_eq!(resp.tool_calls.len(), 1);
        let call = &resp.tool_calls[0];
        assert_eq!(call.id, "tu_1");
        assert_eq!(call.name, "search");
        assert_eq!(call.arguments, json!({"q": "rust"}));
        assert!(matches!(&deltas[0], ChatDelta::ToolCall(c) if c.id == "tu_1"));
    }

    #[test]
    fn empty_tool_args_default_to_object() {
        let events = vec![
            json!({"type": "content_block_start", "content_block": {"type": "tool_use", "id": "t", "name": "noop"}}),
            json!({"type": "content_block_stop"}),
            json!({"type": "message_stop"}),
        ];
        let (_, resp, _) = run(&events);
        assert_eq!(resp.tool_calls[0].arguments, json!({}));
    }

    #[test]
    fn malformed_tool_args_wrap_raw() {
        let events = vec![
            json!({"type": "content_block_start", "content_block": {"type": "tool_use", "id": "t", "name": "noop"}}),
            json!({"type": "content_block_delta", "delta": {"type": "input_json_delta", "partial_json": "{not json"}}),
            json!({"type": "content_block_stop"}),
            json!({"type": "message_stop"}),
        ];
        let (_, resp, _) = run(&events);
        assert_eq!(resp.tool_calls[0].arguments, json!({"_raw": "{not json"}));
    }

    #[test]
    fn max_tokens_maps_to_length() {
        let events = vec![
            json!({"type": "message_delta", "delta": {"stop_reason": "max_tokens"}}),
            json!({"type": "message_stop"}),
        ];
        let (_, resp, _) = run(&events);
        assert_eq!(resp.finish_reason, FinishReason::Length);
    }

    #[test]
    fn error_event_surfaces_provider_error() {
        let mut builder = ResponseBuilder::new();
        let mut sink = |_: ChatDelta| {};
        let evt = json!({"type": "error", "error": {"message": "overloaded"}});
        let err = builder.apply(&evt, &mut sink).unwrap_err();
        match err {
            AgentError::Provider(msg) => assert!(msg.contains("overloaded")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn default_reason_is_tool_use_when_calls_present() {
        let events = vec![
            json!({"type": "content_block_start", "content_block": {"type": "tool_use", "id": "t", "name": "noop"}}),
            json!({"type": "content_block_stop"}),
        ];
        let (_, resp, _) = run(&events);
        assert_eq!(resp.finish_reason, FinishReason::ToolUse);
    }
}
