//! Per-turn execution: issuing one provider request, streaming its deltas, and
//! dispatching the tool calls it asks for. These are the inner steps the run
//! loop in [`super`] drives; they are split out to keep each file focused.

use std::borrow::Cow;

use nerve_core::CancelToken;

use crate::error::{AgentError, AgentResult};
use crate::message::{
    ChatDelta, ChatRequest, ChatResponse, Message, Reasoning, Role, ToolCall, ToolSpec, Usage,
};

use super::{AgentEvent, Orchestrator};

/// When this many turns (or fewer) remain, warn the model so it wraps up instead
/// of being abruptly cut off at `max_turns`.
const TURN_BUDGET_WARN: u32 = 3;

/// Near the turn cap, append a heads-up so the model finishes and reports partial
/// progress instead of being cut off at `max_turns`. Returns the prompt unchanged
/// when there is ample budget (or the cap is itself tiny).
pub(super) fn turn_budget_prompt(system: &str, remaining: u32, max_turns: u32) -> Cow<'_, str> {
    if max_turns <= TURN_BUDGET_WARN || remaining > TURN_BUDGET_WARN {
        return Cow::Borrowed(system);
    }
    Cow::Owned(format!(
        "{system}\n\n[{remaining} turn(s) left before this run ends. Prioritize finishing \
         and reporting the task; don't start work you can't complete in time.]"
    ))
}

impl Orchestrator<'_> {
    /// Tool specs advertised to the model, narrowed by `def.tool_filter`.
    pub(super) fn filtered_tools(&self) -> Vec<ToolSpec> {
        let specs = self.toolbox.specs();
        match &self.def.tool_filter {
            None => specs,
            Some(allow) => specs
                .into_iter()
                .filter(|spec| allow.iter().any(|name| name == &spec.name))
                .collect(),
        }
    }

    /// Issue one provider request, forwarding streamed deltas as events, then
    /// append the assistant message to history and return the assembled reply.
    ///
    /// Before sending, an obvious overflow is pre-empted by truncating history
    /// down toward the context window. If the provider still reports a
    /// context-window overflow, the single largest tool result is truncated and
    /// the turn is retried once (see [`super::overflow`]).
    pub(super) fn execute_turn(
        &mut self,
        system_prompt: &str,
        tools: &[ToolSpec],
        cancel: &CancelToken,
        sink: &mut dyn FnMut(AgentEvent),
    ) -> AgentResult<ChatResponse> {
        self.preempt_overflow();
        let response = self.send_with_overflow_retry(system_prompt, tools, cancel, sink)?;

        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }

        self.history.push(Message {
            role: Role::Assistant,
            content: response.content.clone(),
            // Carry the reasoning + signature so the next turn can replay the
            // thinking block (Anthropic requires the signature verbatim).
            reasoning: reasoning_from(&response),
            tool_calls: response.tool_calls.clone(),
            tool_call_id: None,
            name: None,
        });
        Ok(response)
    }

    /// Send the current history once, and — if the provider reports a
    /// context-window overflow — truncate the largest tool result and retry a
    /// single time. Every other error propagates unchanged.
    fn send_with_overflow_retry(
        &mut self,
        system_prompt: &str,
        tools: &[ToolSpec],
        cancel: &CancelToken,
        sink: &mut dyn FnMut(AgentEvent),
    ) -> AgentResult<ChatResponse> {
        match self.send_request(system_prompt, tools, cancel, sink) {
            Ok(response) => Ok(response),
            Err(err) if super::overflow::is_context_overflow(&err) => {
                if super::overflow::truncate_largest_tool_result(&mut self.history) {
                    self.truncations += 1;
                    self.send_request(system_prompt, tools, cancel, sink)
                } else {
                    Err(err)
                }
            }
            Err(err) => Err(err),
        }
    }

    /// Pre-empt an obvious overflow: if the estimated history footprint already
    /// exceeds what the context window can hold alongside the response reserve,
    /// run the deterministic stub pass down to that hard ceiling before sending.
    /// This is best-effort — the provider-side retry still backstops a miss.
    fn preempt_overflow(&mut self) {
        let ceiling = self
            .caps
            .context_window
            .saturating_sub(self.caps.max_output_tokens as usize);
        if super::compaction::history_tokens(&self.history) > ceiling {
            super::compaction::compact_history(&mut self.history, ceiling);
        }
    }

    /// Build and send one provider request against the current history.
    fn send_request(
        &self,
        system_prompt: &str,
        tools: &[ToolSpec],
        cancel: &CancelToken,
        sink: &mut dyn FnMut(AgentEvent),
    ) -> AgentResult<ChatResponse> {
        let mut req = ChatRequest {
            model: self.def.model.clone(),
            system: Some(system_prompt.to_string()),
            messages: self.history.clone(),
            tools: tools.to_vec(),
            temperature: self.def.temperature,
            max_tokens: Some(self.caps.max_output_tokens),
            reasoning_effort: self.def.reasoning_effort.clone(),
        };
        for hook in &self.hooks {
            hook.on_request(&mut req);
        }
        self.provider.chat(&req, cancel, &mut |delta| match delta {
            ChatDelta::Text(text) => sink(AgentEvent::AssistantText(text)),
            ChatDelta::Reasoning(text) => sink(AgentEvent::Reasoning(text)),
            // Advisory streaming only: dispatch uses the assembled call from the
            // response, so this never affects tool execution.
            ChatDelta::ToolCall(call) => sink(AgentEvent::ToolCallDelta {
                name: call.name,
                arguments: call.arguments,
            }),
        })
    }

    /// Run every requested tool call, emitting lifecycle events and appending a
    /// `Tool` result message per call. Returns `Some(reason)` when the failure
    /// guardrail trips and the run should stop.
    pub(super) fn dispatch_tool_calls(
        &mut self,
        calls: &[ToolCall],
        cancel: &CancelToken,
        sink: &mut dyn FnMut(AgentEvent),
        consecutive_failures: &mut u32,
    ) -> AgentResult<Option<String>> {
        for call in calls {
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            sink(AgentEvent::ToolStarted {
                name: call.name.clone(),
                args: call.arguments.clone(),
            });

            let (ok, output) = match self.toolbox.call(&call.name, &call.arguments, cancel) {
                Ok(value) => (true, value_to_string(&value)),
                Err(err) => (false, format!("error: {err}")),
            };
            sink(AgentEvent::ToolFinished {
                name: call.name.clone(),
                ok,
                output: output.clone(),
            });
            if ok {
                *consecutive_failures = 0;
                self.history
                    .push(Message::tool(call.id.clone(), call.name.clone(), output));
                continue;
            }

            *consecutive_failures += 1;
            if let Some(limit) = self.def.max_tool_failures
                && *consecutive_failures > limit
            {
                self.history
                    .push(Message::tool(call.id.clone(), call.name.clone(), output));
                return Ok(Some(format!("max_tool_failures ({limit}) exceeded")));
            }
            // From the second consecutive failure on, surface the remaining failure
            // budget so the model changes approach instead of repeating a failing call.
            let body = failure_feedback(output, *consecutive_failures, self.def.max_tool_failures);
            self.history
                .push(Message::tool(call.id.clone(), call.name.clone(), body));
        }
        Ok(None)
    }
}

/// Append an adaptive nudge to a failed tool result once failures repeat, so the
/// model changes approach instead of looping on the same call. The first failure
/// passes through untouched (it may just be transient).
fn failure_feedback(output: String, consecutive_failures: u32, limit: Option<u32>) -> String {
    if consecutive_failures < 2 {
        return output;
    }
    match limit {
        Some(limit) => format!(
            "{output}\n\n[{consecutive_failures} consecutive tool failures; {} attempt(s) \
             left before this run aborts. Try a DIFFERENT approach — do not repeat the \
             same call.]",
            limit + 1 - consecutive_failures,
        ),
        None => format!(
            "{output}\n\n[{consecutive_failures} consecutive tool failures; try a DIFFERENT \
             approach rather than repeating the same call.]"
        ),
    }
}

/// Build the [`Reasoning`] to store on the assistant message from a response,
/// or `None` when the model produced no reasoning text. The signature (if any)
/// is carried alongside so a provider that requires it can replay it verbatim.
fn reasoning_from(response: &ChatResponse) -> Option<Reasoning> {
    let text = response.reasoning.clone()?;
    Some(Reasoning {
        text,
        signature: response.reasoning_signature.clone(),
    })
}

/// Render a successful tool result as a string: pass JSON strings through
/// verbatim, serialize anything else compactly.
fn value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

/// Add a response's token counts into the running total, saturating on overflow.
/// All four fields fold (including the cache fields), so the aggregated
/// `RunOutcome.usage` and the cost hook see real cache_read / cache_creation
/// totals rather than always-zero.
pub(super) fn accumulate_usage(total: &mut Usage, delta: &Usage) {
    total.input_tokens = total.input_tokens.saturating_add(delta.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(delta.output_tokens);
    total.cache_read_tokens = total
        .cache_read_tokens
        .saturating_add(delta.cache_read_tokens);
    total.cache_creation_tokens = total
        .cache_creation_tokens
        .saturating_add(delta.cache_creation_tokens);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulate_usage_folds_all_four_fields() {
        let mut total = Usage::default();
        accumulate_usage(
            &mut total,
            &Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 7,
                cache_creation_tokens: 3,
            },
        );
        accumulate_usage(
            &mut total,
            &Usage {
                input_tokens: 20,
                output_tokens: 8,
                cache_read_tokens: 1,
                cache_creation_tokens: 4,
            },
        );
        assert_eq!(total.input_tokens, 30);
        assert_eq!(total.output_tokens, 13);
        // Cache fields must accumulate too — previously they stayed 0.
        assert_eq!(total.cache_read_tokens, 8);
        assert_eq!(total.cache_creation_tokens, 7);
    }

    #[test]
    fn accumulate_usage_saturates_on_overflow() {
        let mut total = Usage {
            cache_read_tokens: u32::MAX,
            cache_creation_tokens: u32::MAX,
            ..Default::default()
        };
        accumulate_usage(
            &mut total,
            &Usage {
                cache_read_tokens: 5,
                cache_creation_tokens: 5,
                ..Default::default()
            },
        );
        assert_eq!(total.cache_read_tokens, u32::MAX);
        assert_eq!(total.cache_creation_tokens, u32::MAX);
    }
}
