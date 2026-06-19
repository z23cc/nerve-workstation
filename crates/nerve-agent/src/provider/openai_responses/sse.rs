//! Incremental parsing of the OpenAI Responses API SSE stream.
//!
//! Mirrors `_parse_openai_sse` (responses mode) from the reference
//! `GenericAgent/llmcore.py`. An [`Assembler`] is fed one decoded event JSON at
//! a time via [`Assembler::handle_event`], emitting [`ChatDelta`]s into the
//! sink, and is finalized into a [`ChatResponse`] via [`Assembler::finish`].
//!
//! Event types consumed:
//! - `response.output_text.delta` → assistant text delta.
//! - `response.output_text.done` → fallback text if no delta was seen.
//! - `response.reasoning_summary_text.delta` / `response.reasoning_summary.delta`
//!   → reasoning (thinking) delta.
//! - `response.output_item.added` (function_call) → begin a tool-call buffer.
//! - `response.function_call_arguments.delta` → accumulate call arguments.
//! - `response.function_call_arguments.done` → finalize call arguments.
//! - `response.completed` → usage + status → finish reason.
//! - `error` → surfaced as [`AgentError::Provider`].

use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::{AgentError, AgentResult};
use crate::message::{ChatDelta, ChatResponse, FinishReason, ToolCall, Usage};

/// Whether the caller should keep reading events or stop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Flow {
    /// Continue reading the stream.
    Continue,
    /// Terminal event seen (`response.completed`); stop reading.
    Stop,
}

/// An in-progress function call accumulated across streamed events.
#[derive(Default)]
struct CallBuffer {
    id: String,
    name: String,
    arguments: String,
}

/// Accumulates streamed Responses events into a [`ChatResponse`].
#[derive(Default)]
pub(super) struct Assembler {
    content: String,
    reasoning: String,
    seen_text_delta: bool,
    /// Function-call buffers keyed by `output_index` (preserves emission order).
    calls: BTreeMap<u64, CallBuffer>,
    last_call_index: Option<u64>,
    usage: Usage,
    finish_reason: Option<FinishReason>,
}

impl Assembler {
    /// Process one decoded SSE event JSON, emitting deltas into `sink`.
    pub(super) fn handle_event(
        &mut self,
        event: &Value,
        sink: &mut dyn FnMut(ChatDelta),
    ) -> AgentResult<Flow> {
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "response.output_text.delta" => {
                self.on_text_delta(event, sink);
                Ok(Flow::Continue)
            }
            "response.output_text.done" => {
                self.on_text_done(event, sink);
                Ok(Flow::Continue)
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_summary.delta" => {
                self.on_reasoning_delta(event, sink);
                Ok(Flow::Continue)
            }
            "response.output_item.added" => {
                self.on_item_added(event);
                Ok(Flow::Continue)
            }
            "response.function_call_arguments.delta" => {
                self.on_arguments_delta(event);
                Ok(Flow::Continue)
            }
            "response.function_call_arguments.done" => {
                self.on_arguments_done(event);
                Ok(Flow::Continue)
            }
            "response.completed" => {
                self.on_completed(event);
                Ok(Flow::Stop)
            }
            "error" => Err(stream_error(event)),
            // Lifecycle events we don't need (created, in_progress, item.done, …).
            _ => Ok(Flow::Continue),
        }
    }

    /// Assemble the terminal [`ChatResponse`]. Defaults the finish reason to
    /// `ToolUse` when calls were produced, otherwise `Stop`.
    pub(super) fn finish(self) -> ChatResponse {
        let tool_calls = self.collect_tool_calls();
        let finish_reason = self.finish_reason.unwrap_or({
            if tool_calls.is_empty() {
                FinishReason::Stop
            } else {
                FinishReason::ToolUse
            }
        });
        ChatResponse {
            content: self.content,
            reasoning: (!self.reasoning.is_empty()).then_some(self.reasoning),
            // The Responses API does not return a replayable reasoning signature.
            reasoning_signature: None,
            tool_calls,
            finish_reason,
            usage: self.usage,
        }
    }

    fn on_text_delta(&mut self, event: &Value, sink: &mut dyn FnMut(ChatDelta)) {
        if let Some(delta) = event.get("delta").and_then(Value::as_str)
            && !delta.is_empty()
        {
            self.seen_text_delta = true;
            self.content.push_str(delta);
            sink(ChatDelta::Text(delta.to_string()));
        }
    }

    fn on_text_done(&mut self, event: &Value, sink: &mut dyn FnMut(ChatDelta)) {
        if self.seen_text_delta {
            return;
        }
        if let Some(text) = event.get("text").and_then(Value::as_str)
            && !text.is_empty()
        {
            self.content.push_str(text);
            sink(ChatDelta::Text(text.to_string()));
        }
    }

    fn on_reasoning_delta(&mut self, event: &Value, sink: &mut dyn FnMut(ChatDelta)) {
        if let Some(delta) = event.get("delta").and_then(Value::as_str)
            && !delta.is_empty()
        {
            self.reasoning.push_str(delta);
            sink(ChatDelta::Reasoning(delta.to_string()));
        }
    }

    fn on_item_added(&mut self, event: &Value) {
        let item = event.get("item").unwrap_or(&Value::Null);
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            return;
        }
        let index = output_index(event).unwrap_or(0);
        let id = item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        self.calls.insert(
            index,
            CallBuffer {
                id,
                name,
                arguments: String::new(),
            },
        );
        self.last_call_index = Some(index);
    }

    fn on_arguments_delta(&mut self, event: &Value) {
        let index = self.resolve_index(event);
        if let (Some(buffer), Some(delta)) = (
            self.calls.get_mut(&index),
            event.get("delta").and_then(Value::as_str),
        ) {
            buffer.arguments.push_str(delta);
        }
    }

    fn on_arguments_done(&mut self, event: &Value) {
        let index = self.resolve_index(event);
        if let (Some(buffer), Some(arguments)) = (
            self.calls.get_mut(&index),
            event.get("arguments").and_then(Value::as_str),
        ) {
            buffer.arguments = arguments.to_string();
        }
    }

    fn on_completed(&mut self, event: &Value) {
        let response = event.get("response").unwrap_or(&Value::Null);
        if let Some(usage) = response.get("usage") {
            self.usage = parse_usage(usage);
        }
        let status = response.get("status").and_then(Value::as_str);
        self.finish_reason = Some(finish_reason_for(status, &self.calls));
    }

    /// Resolve the call index for an arguments event, falling back to the most
    /// recently added function-call buffer when `output_index` is absent.
    fn resolve_index(&self, event: &Value) -> u64 {
        output_index(event).or(self.last_call_index).unwrap_or(0)
    }

    fn collect_tool_calls(&self) -> Vec<ToolCall> {
        self.calls
            .values()
            .filter(|buffer| !buffer.name.is_empty())
            .map(|buffer| ToolCall {
                id: buffer.id.clone(),
                name: buffer.name.clone(),
                arguments: parse_arguments(&buffer.arguments),
            })
            .collect()
    }
}

/// Parse accumulated argument text into a JSON object. Empty input yields an
/// empty object; unparseable input is preserved under `_raw`.
fn parse_arguments(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return Value::Object(serde_json::Map::new());
    }
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::json!({ "_raw": raw }))
}

/// Map a Responses `status` plus pending calls to a [`FinishReason`].
fn finish_reason_for(status: Option<&str>, calls: &BTreeMap<u64, CallBuffer>) -> FinishReason {
    match status {
        Some("incomplete") => FinishReason::Length,
        Some("completed") | None => {
            if calls.values().any(|buffer| !buffer.name.is_empty()) {
                FinishReason::ToolUse
            } else {
                FinishReason::Stop
            }
        }
        Some(other) => FinishReason::Other(other.to_string()),
    }
}

/// Token accounting from a Responses `usage` object. Cached prompt tokens are
/// reported under `input_tokens_details.cached_tokens`; OpenAI does not surface a
/// separate cache-creation count, so that stays `0`.
fn parse_usage(usage: &Value) -> Usage {
    let cache_read_tokens = usage
        .get("input_tokens_details")
        .map_or(0, |details| field_u32(details, "cached_tokens"));
    Usage {
        input_tokens: field_u32(usage, "input_tokens"),
        output_tokens: field_u32(usage, "output_tokens"),
        cache_read_tokens,
        cache_creation_tokens: 0,
    }
}

/// Read a non-negative integer `field` from `value`, saturating into `u32`.
fn field_u32(value: &Value, field: &str) -> u32 {
    let raw = value.get(field).and_then(Value::as_u64).unwrap_or(0);
    u32::try_from(raw).unwrap_or(u32::MAX)
}

/// Extract a numeric `output_index` field from an event.
fn output_index(event: &Value) -> Option<u64> {
    event.get("output_index").and_then(Value::as_u64)
}

/// Build an [`AgentError`] from a Responses `error` event.
fn stream_error(event: &Value) -> AgentError {
    let error = event.get("error").unwrap_or(&Value::Null);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| error.to_string());
    AgentError::Provider(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn drive(events: &[Value]) -> AgentResult<(ChatResponse, Vec<ChatDelta>)> {
        let mut assembler = Assembler::default();
        let mut deltas = Vec::new();
        for event in events {
            let mut sink = |delta: ChatDelta| deltas.push(delta);
            if assembler.handle_event(event, &mut sink)? == Flow::Stop {
                break;
            }
        }
        Ok((assembler.finish(), deltas))
    }

    #[test]
    fn accumulates_text_deltas() {
        let events = vec![
            json!({ "type": "response.output_text.delta", "delta": "Hel" }),
            json!({ "type": "response.output_text.delta", "delta": "lo" }),
            json!({ "type": "response.completed", "response": { "status": "completed" } }),
        ];
        let (response, deltas) = drive(&events).unwrap();
        assert_eq!(response.content, "Hello");
        assert_eq!(response.finish_reason, FinishReason::Stop);
        assert_eq!(deltas.len(), 2);
        assert!(matches!(&deltas[0], ChatDelta::Text(t) if t == "Hel"));
    }

    #[test]
    fn output_text_done_is_fallback_only() {
        // With deltas present, the `.done` text must not be appended again.
        let events = vec![
            json!({ "type": "response.output_text.delta", "delta": "hi" }),
            json!({ "type": "response.output_text.done", "text": "hi" }),
        ];
        let (response, _) = drive(&events).unwrap();
        assert_eq!(response.content, "hi");
    }

    #[test]
    fn output_text_done_used_when_no_delta() {
        let events = vec![json!({ "type": "response.output_text.done", "text": "final" })];
        let (response, deltas) = drive(&events).unwrap();
        assert_eq!(response.content, "final");
        assert_eq!(deltas.len(), 1);
    }

    #[test]
    fn reasoning_summary_deltas_emit_reasoning() {
        let events = vec![
            json!({ "type": "response.reasoning_summary_text.delta", "delta": "think " }),
            json!({ "type": "response.reasoning_summary_text.delta", "delta": "more" }),
        ];
        let (response, deltas) = drive(&events).unwrap();
        assert_eq!(response.reasoning.as_deref(), Some("think more"));
        assert!(matches!(&deltas[0], ChatDelta::Reasoning(t) if t == "think "));
    }

    #[test]
    fn assembles_function_call() {
        let events = vec![
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "function_call", "call_id": "call_42", "name": "search" }
            }),
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 0,
                "delta": "{\"q\":"
            }),
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 0,
                "delta": "\"rust\"}"
            }),
            json!({ "type": "response.completed", "response": { "status": "completed" } }),
        ];
        let (response, _) = drive(&events).unwrap();
        assert_eq!(response.finish_reason, FinishReason::ToolUse);
        assert_eq!(response.tool_calls.len(), 1);
        let call = &response.tool_calls[0];
        assert_eq!(call.id, "call_42");
        assert_eq!(call.name, "search");
        assert_eq!(call.arguments, json!({ "q": "rust" }));
    }

    #[test]
    fn arguments_done_overrides_accumulated() {
        let events = vec![
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "function_call", "call_id": "c", "name": "t" }
            }),
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 0,
                "delta": "{\"partial\""
            }),
            json!({
                "type": "response.function_call_arguments.done",
                "output_index": 0,
                "arguments": "{\"final\":true}"
            }),
        ];
        let (response, _) = drive(&events).unwrap();
        assert_eq!(response.tool_calls[0].arguments, json!({ "final": true }));
    }

    #[test]
    fn multiple_calls_preserve_index_order() {
        let events = vec![
            json!({
                "type": "response.output_item.added",
                "output_index": 1,
                "item": { "type": "function_call", "call_id": "b", "name": "second" }
            }),
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "function_call", "call_id": "a", "name": "first" }
            }),
        ];
        let (response, _) = drive(&events).unwrap();
        assert_eq!(response.tool_calls.len(), 2);
        assert_eq!(response.tool_calls[0].name, "first");
        assert_eq!(response.tool_calls[1].name, "second");
    }

    #[test]
    fn completed_parses_usage() {
        let events = vec![json!({
            "type": "response.completed",
            "response": {
                "status": "completed",
                "usage": { "input_tokens": 12, "output_tokens": 7 }
            }
        })];
        let (response, _) = drive(&events).unwrap();
        assert_eq!(response.usage.input_tokens, 12);
        assert_eq!(response.usage.output_tokens, 7);
    }

    #[test]
    fn incomplete_status_is_length() {
        let events = vec![json!({
            "type": "response.completed",
            "response": { "status": "incomplete" }
        })];
        let (response, _) = drive(&events).unwrap();
        assert_eq!(response.finish_reason, FinishReason::Length);
    }

    #[test]
    fn unknown_status_is_other() {
        let events = vec![json!({
            "type": "response.completed",
            "response": { "status": "failed" }
        })];
        let (response, _) = drive(&events).unwrap();
        assert_eq!(response.finish_reason, FinishReason::Other("failed".into()));
    }

    #[test]
    fn error_event_is_provider_error() {
        let mut assembler = Assembler::default();
        let event = json!({ "type": "error", "error": { "message": "boom" } });
        let mut sink = |_: ChatDelta| {};
        let err = assembler.handle_event(&event, &mut sink).unwrap_err();
        assert!(matches!(err, AgentError::Provider(m) if m == "boom"));
    }

    #[test]
    fn arguments_delta_without_index_uses_last_call() {
        let events = vec![
            json!({
                "type": "response.output_item.added",
                "output_index": 3,
                "item": { "type": "function_call", "call_id": "c", "name": "t" }
            }),
            // No output_index on the delta — should target the last-added call.
            json!({
                "type": "response.function_call_arguments.delta",
                "delta": "{\"k\":1}"
            }),
        ];
        let (response, _) = drive(&events).unwrap();
        assert_eq!(response.tool_calls[0].arguments, json!({ "k": 1 }));
    }

    #[test]
    fn unparseable_arguments_preserved_as_raw() {
        let events = vec![
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "type": "function_call", "call_id": "c", "name": "t" }
            }),
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 0,
                "delta": "not json"
            }),
        ];
        let (response, _) = drive(&events).unwrap();
        assert_eq!(
            response.tool_calls[0].arguments,
            json!({ "_raw": "not json" })
        );
    }

    #[test]
    fn empty_arguments_become_empty_object() {
        let events = vec![json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": { "type": "function_call", "call_id": "c", "name": "t" }
        })];
        let (response, _) = drive(&events).unwrap();
        assert_eq!(response.tool_calls[0].arguments, json!({}));
    }
}
