//! Defensive text-fallback recovery of tool calls.
//!
//! Most providers emit native tool-call blocks, but some models (or degraded
//! responses) instead *describe* a call inline in the assistant text — either as
//! an Anthropic-style `<tool_use>{...}</tool_use>` element or as a fenced
//! ```json code block. When a turn yields **no** native tool calls, this module
//! recovers any such textual calls so the orchestrator can still dispatch them.
//!
//! This is intentionally narrow — two well-known shapes, not a per-model dialect
//! framework. It runs only as a fallback (native calls always win), so a normal
//! response that merely *mentions* JSON is never misread: recovery requires the
//! exact wrapper shapes below and a parseable `name` + object `arguments`.

use serde_json::Value;

use crate::message::ToolCall;

/// Recover tool calls described inline in `content`, in document order.
///
/// Returns an empty vec when nothing matches. Callers apply this **only** when a
/// response carried no native tool calls.
#[must_use]
pub(crate) fn recover_tool_calls(content: &str) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    collect_tagged(content, &mut calls);
    if calls.is_empty() {
        collect_fenced_json(content, &mut calls);
    }
    calls
}

/// Recover calls from `<tool_use> … </tool_use>` elements whose body is a JSON
/// object with a `name` and (optional) `input`/`arguments`/`parameters`.
fn collect_tagged(content: &str, out: &mut Vec<ToolCall>) {
    let mut rest = content;
    while let Some(open) = rest.find("<tool_use>") {
        let after = &rest[open + "<tool_use>".len()..];
        let Some(close) = after.find("</tool_use>") else {
            break;
        };
        let body = &after[..close];
        if let Some(call) = call_from_json_text(body) {
            out.push(call);
        }
        rest = &after[close + "</tool_use>".len()..];
    }
}

/// Recover calls from fenced ```json … ``` blocks whose payload is a tool-call
/// object. Only the first parseable call per block is taken.
fn collect_fenced_json(content: &str, out: &mut Vec<ToolCall>) {
    let mut rest = content;
    while let Some(start) = rest.find("```") {
        let after = &rest[start + 3..];
        // Skip an optional language tag up to the first newline.
        let body_start = after.find('\n').map_or(after.len(), |nl| nl + 1);
        let body_region = &after[body_start..];
        let Some(end) = body_region.find("```") else {
            break;
        };
        let body = &body_region[..end];
        if let Some(call) = call_from_json_text(body) {
            out.push(call);
        }
        rest = &body_region[end + 3..];
    }
}

/// Parse a JSON object describing a tool call: a string `name` plus an arguments
/// object under `input`, `arguments`, or `parameters` (defaulting to `{}`).
/// Returns `None` unless the text parses to an object with a non-empty name.
fn call_from_json_text(text: &str) -> Option<ToolCall> {
    let value: Value = serde_json::from_str(text.trim()).ok()?;
    let obj = value.as_object()?;
    let name = obj.get("name")?.as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    let arguments = obj
        .get("input")
        .or_else(|| obj.get("arguments"))
        .or_else(|| obj.get("parameters"))
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    Some(ToolCall {
        // Textual calls have no provider id; a stable synthetic id keeps the
        // result correlated to this call within the turn.
        id: format!("text_fallback_{name}"),
        name: name.to_string(),
        arguments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recovers_tool_use_xml_element() {
        let content = "I'll search.\n<tool_use>{\"name\": \"search\", \
            \"input\": {\"q\": \"rust\"}}</tool_use>";
        let calls = recover_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "search");
        assert_eq!(calls[0].arguments, json!({"q": "rust"}));
    }

    #[test]
    fn recovers_multiple_tagged_calls_in_order() {
        let content = "<tool_use>{\"name\":\"a\",\"input\":{}}</tool_use>\
            <tool_use>{\"name\":\"b\",\"arguments\":{\"x\":1}}</tool_use>";
        let calls = recover_tool_calls(content);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
        assert_eq!(calls[1].arguments, json!({"x": 1}));
    }

    #[test]
    fn recovers_fenced_json_block() {
        let content = "Here is the call:\n```json\n{\"name\": \"read_file\", \
            \"parameters\": {\"path\": \"a.rs\"}}\n```";
        let calls = recover_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, json!({"path": "a.rs"}));
    }

    #[test]
    fn fenced_block_without_language_tag_is_parsed() {
        let content = "```\n{\"name\":\"noop\"}\n```";
        let calls = recover_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "noop");
        // Missing arguments default to an empty object.
        assert_eq!(calls[0].arguments, json!({}));
    }

    #[test]
    fn tagged_calls_take_precedence_over_fenced() {
        // A response with both shapes recovers the explicit tool_use elements
        // only, not the prose-y fenced block, to avoid double-dispatch.
        let content = "<tool_use>{\"name\":\"tagged\",\"input\":{}}</tool_use>\n\
            ```json\n{\"name\":\"fenced\"}\n```";
        let calls = recover_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "tagged");
    }

    #[test]
    fn plain_prose_recovers_nothing() {
        assert!(recover_tool_calls("Just a normal answer with no tools.").is_empty());
        // A fenced block that isn't a tool call is ignored.
        assert!(recover_tool_calls("```json\n{\"result\": 42}\n```").is_empty());
        // An empty name is rejected.
        assert!(recover_tool_calls("<tool_use>{\"name\":\"\"}</tool_use>").is_empty());
    }

    #[test]
    fn malformed_json_in_wrapper_is_ignored() {
        assert!(recover_tool_calls("<tool_use>{not json}</tool_use>").is_empty());
    }
}
