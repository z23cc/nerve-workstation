//! LLM provider abstraction and the shared HTTP/SSE helpers.
//!
//! Each concrete provider (`anthropic`, `openai_responses`, `xai`) implements
//! [`LlmProvider`] over the shared, blocking [`http`] helpers. Tools available
//! to the model are exposed through a [`ToolBox`].

use nerve_core::CancelToken;

use crate::AgentResult;
use crate::auth::ProviderId;
use crate::message::{ChatDelta, ChatRequest, ChatResponse, FinishReason, ToolSpec};

pub mod anthropic;
pub mod http;
pub mod openai_responses;
pub(crate) mod retry;
pub(crate) mod text_fallback;
pub mod xai;

/// Recover textually-expressed tool calls when a response carried none natively.
///
/// A no-op when the response already has native tool calls (those always win).
/// Otherwise it scans the assistant text for `<tool_use>` / fenced-JSON calls
/// (see [`text_fallback`]); on a hit the calls are attached and the finish
/// reason is promoted to [`FinishReason::ToolUse`] so the loop dispatches them.
pub(crate) fn apply_text_fallback(mut response: ChatResponse) -> ChatResponse {
    if !response.tool_calls.is_empty() {
        return response;
    }
    let recovered = text_fallback::recover_tool_calls(&response.content);
    if !recovered.is_empty() {
        response.tool_calls = recovered;
        response.finish_reason = FinishReason::ToolUse;
    }
    response
}

/// A streaming chat-completion provider for one vendor.
pub trait LlmProvider: Send + Sync {
    /// The provider this implementation talks to.
    fn id(&self) -> ProviderId;

    /// Run a chat completion, streaming deltas into `sink` and returning the
    /// assembled response. Implementations must honor `cancel`.
    fn chat(
        &self,
        req: &ChatRequest,
        cancel: &CancelToken,
        sink: &mut dyn FnMut(ChatDelta),
    ) -> AgentResult<ChatResponse>;
}

/// A set of callable tools exposed to the model.
pub trait ToolBox: Send + Sync {
    /// The specs advertised to the model.
    fn specs(&self) -> Vec<ToolSpec>;

    /// Invoke the tool `name` with `args`, honoring `cancel`.
    fn call(
        &self,
        name: &str,
        args: &serde_json::Value,
        cancel: &CancelToken,
    ) -> AgentResult<serde_json::Value>;
}

#[cfg(test)]
mod fallback_tests {
    use super::*;
    use crate::message::{ToolCall, Usage};

    fn resp(content: &str, tool_calls: Vec<ToolCall>, finish: FinishReason) -> ChatResponse {
        ChatResponse {
            content: content.into(),
            reasoning: None,
            reasoning_signature: None,
            tool_calls,
            finish_reason: finish,
            usage: Usage::default(),
        }
    }

    #[test]
    fn native_tool_calls_are_left_untouched() {
        let native = vec![ToolCall {
            id: "real".into(),
            name: "search".into(),
            arguments: serde_json::json!({}),
        }];
        let out = apply_text_fallback(resp(
            "<tool_use>{\"name\":\"other\"}</tool_use>",
            native,
            FinishReason::ToolUse,
        ));
        // The native call wins; the textual one is ignored.
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].id, "real");
    }

    #[test]
    fn textual_call_is_recovered_and_finish_reason_promoted() {
        let out = apply_text_fallback(resp(
            "I'll do it.\n<tool_use>{\"name\":\"search\",\"input\":{\"q\":\"x\"}}</tool_use>",
            Vec::new(),
            FinishReason::Stop,
        ));
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].name, "search");
        // Recovery flips Stop -> ToolUse so the orchestrator dispatches the call.
        assert_eq!(out.finish_reason, FinishReason::ToolUse);
    }

    #[test]
    fn plain_text_response_is_unchanged() {
        let out = apply_text_fallback(resp("just an answer", Vec::new(), FinishReason::Stop));
        assert!(out.tool_calls.is_empty());
        assert_eq!(out.finish_reason, FinishReason::Stop);
    }
}
