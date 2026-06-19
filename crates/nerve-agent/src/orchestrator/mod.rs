//! The agentic run loop: drives a provider through tool-use turns.
//!
//! This module defines the configuration ([`AgentDef`]), the streamed
//! [`AgentEvent`]s, and the final [`RunOutcome`], plus the [`Orchestrator`] that
//! holds a borrowed provider/toolbox and drives the LLM/tool loop in
//! [`Orchestrator::run`]. The per-turn mechanics live in [`turn`] and the
//! history-bounding pass in [`compaction`].

mod capabilities;
mod compaction;
mod overflow;
mod turn;

pub use capabilities::ModelCapabilities;

use nerve_core::CancelToken;

use crate::error::{AgentError, AgentResult};
use crate::message::{FinishReason, Message, Usage};
use crate::provider::{LlmProvider, ToolBox};

use turn::{accumulate_usage, turn_budget_prompt};

/// Pushed once (opt-in) when the model first signals completion, prompting it to
/// re-verify the result before the run ends.
const COMPLETION_VERIFY_PROMPT: &str = "Before finishing: double-check that you actually \
accomplished the task — re-verify the key result against the original request. If anything is \
incomplete or unverified, keep working (call tools to finish or check); if it is genuinely done, \
give your final answer.";

/// Static configuration for an agent run.
#[derive(Clone, Debug)]
pub struct AgentDef {
    /// System prompt prepended to every request.
    pub system_prompt: String,
    /// Model identifier to use.
    pub model: String,
    /// Maximum number of turns before the run is stopped.
    pub max_turns: u32,
    /// Cap on model requests within a single turn.
    pub max_requests_per_turn: Option<u32>,
    /// Cap on consecutive tool failures before aborting.
    pub max_tool_failures: Option<u32>,
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// Reasoning effort hint passed to the provider.
    pub reasoning_effort: Option<String>,
    /// Optional allowlist of tool names; `None` means all tools.
    pub tool_filter: Option<Vec<String>>,
    /// Opt-in: after the model first signals completion, give it one chance to
    /// re-verify it actually finished (catches premature stops). Off by default.
    pub verify_completion: bool,
}

impl Default for AgentDef {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            model: String::new(),
            max_turns: 40,
            max_requests_per_turn: None,
            max_tool_failures: Some(3),
            temperature: None,
            reasoning_effort: None,
            tool_filter: None,
            verify_completion: false,
        }
    }
}

/// An event streamed out of [`Orchestrator::run`].
#[derive(Clone, Debug)]
pub enum AgentEvent {
    /// A new turn started (1-based turn index).
    TurnStarted(u32),
    /// A chunk of assistant text.
    AssistantText(String),
    /// A chunk of reasoning text.
    Reasoning(String),
    /// A tool invocation began.
    ToolStarted {
        name: String,
        args: serde_json::Value,
    },
    /// A tool invocation finished.
    ToolFinished {
        name: String,
        ok: bool,
        output: String,
    },
    /// The run was interrupted (cancellation or guardrail) with a reason.
    Interrupted(String),
    /// Token usage from the latest provider response (per-response delta).
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        /// Prompt tokens served from the provider's prompt cache (`0` if unreported).
        cache_read_tokens: u32,
        /// Prompt tokens written into the provider's prompt cache (`0` if unreported).
        cache_creation_tokens: u32,
    },
    /// An advisory fragment of an in-progress tool call (streaming), forwarded
    /// for UI only. Dispatch always uses the *assembled* call from the response;
    /// this never drives tool execution.
    ToolCallDelta {
        name: String,
        arguments: serde_json::Value,
    },
    /// The run completed with a terminal reason.
    Done { reason: String },
}

/// The result of a completed agent run.
#[derive(Clone, Debug)]
pub struct RunOutcome {
    /// Terminal reason (e.g. "stop", "max_turns", "cancelled").
    pub reason: String,
    /// Number of turns executed.
    pub turns: u32,
    /// Final assistant text.
    pub final_text: String,
    /// Aggregate token usage across the run.
    pub usage: Usage,
}

/// Restored state for resuming an [`Orchestrator`] from a persisted session.
///
/// Carries the prior transcript so a continued run sees earlier turns —
/// including the *results* of tool calls already executed — without re-running
/// them, plus any run counters worth preserving. Intentionally minimal data:
/// host/session wiring (mapping a persisted checkpoint into these fields) lands
/// in a later wave; this is the seam it will target.
#[derive(Clone, Debug, Default)]
pub struct ResumeState {
    /// Transcript accumulated so far (user/assistant/tool messages).
    pub history: Vec<Message>,
    /// Context-overflow truncations already performed in the prior session.
    pub truncations: u32,
}

/// Observe/augment lifecycle hooks invoked by the [`Orchestrator`] around a run.
///
/// This is an **observe/augment** seam, deliberately distinct from two
/// neighbours: the permission *policy* (which may **veto** a tool call) and the
/// event *sink* (which drives a UI). A hook may rewrite the system prompt and
/// watch the lifecycle, but it can neither block a tool call nor replace the
/// stream. Every method has a no-op default, so the trait is purely additive: a
/// run with no registered hooks behaves exactly as a hook-free run.
///
/// Hooks must not panic: a panic propagates out of the run loop and aborts it.
pub trait Hook: Send + Sync {
    /// Called once before the first request, with mutable access to the system
    /// prompt so the hook may augment it (e.g. inject environment context).
    fn on_start(&self, _system_prompt: &mut String) {}

    /// Called for every provider request after it is built and before it is sent.
    fn on_request(&self, _req: &mut crate::message::ChatRequest) {}

    /// Called for every [`AgentEvent`] as it is streamed out (observe-only).
    fn on_event(&self, _event: &AgentEvent) {}

    /// Called once after a graceful run, with the terminal [`RunOutcome`]. Not
    /// called when the run ends in an error (e.g. cancellation), which yields no
    /// outcome to observe.
    fn on_end(&self, _outcome: &RunOutcome) {}
}

/// Drives a [`LlmProvider`] through tool-use turns against a [`ToolBox`].
pub struct Orchestrator<'a> {
    provider: &'a dyn LlmProvider,
    toolbox: &'a dyn ToolBox,
    def: AgentDef,
    /// Context/output budgets for `def.model`, resolved once at construction.
    caps: ModelCapabilities,
    history: Vec<Message>,
    hooks: Vec<&'a dyn Hook>,
    /// Count of context-overflow truncations performed during the current run.
    truncations: u32,
}

impl<'a> Orchestrator<'a> {
    /// Build an orchestrator over a borrowed provider and toolbox. No lifecycle
    /// hooks are registered; use [`Orchestrator::with_hooks`] to add them.
    pub fn new(provider: &'a dyn LlmProvider, toolbox: &'a dyn ToolBox, def: AgentDef) -> Self {
        let caps = ModelCapabilities::for_model(&def.model);
        Self {
            provider,
            toolbox,
            def,
            caps,
            history: Vec::new(),
            hooks: Vec::new(),
            truncations: 0,
        }
    }

    /// Build an orchestrator resuming a persisted session from `state`.
    ///
    /// The prior transcript (including already-executed tool results) is loaded
    /// so the next [`run`](Self::run) continues from it without re-issuing those
    /// tool calls; restored counters (e.g. truncations) carry over. Equivalent to
    /// [`new`](Self::new) when `state` is [`ResumeState::default`].
    pub fn resume(
        provider: &'a dyn LlmProvider,
        toolbox: &'a dyn ToolBox,
        def: AgentDef,
        state: ResumeState,
    ) -> Self {
        let caps = ModelCapabilities::for_model(&def.model);
        Self {
            provider,
            toolbox,
            def,
            caps,
            history: state.history,
            hooks: Vec::new(),
            truncations: state.truncations,
        }
    }

    /// Number of context-overflow truncations performed so far. A host can
    /// surface this (e.g. to warn that history was lossily shortened).
    #[must_use]
    pub fn truncations(&self) -> u32 {
        self.truncations
    }

    /// Register lifecycle [`Hook`]s, returning `self` for chaining. Hooks fire in
    /// registration order at each lifecycle point. This *replaces* any previously
    /// registered hooks; passing an empty list (the default) leaves the run
    /// hook-free and byte-for-byte unchanged.
    #[must_use]
    pub fn with_hooks(mut self, hooks: Vec<&'a dyn Hook>) -> Self {
        self.hooks = hooks;
        self
    }

    /// Seed the conversation with prior messages before the next [`run`](Self::run).
    ///
    /// This is additive for host-managed sessions: a one-shot run still starts
    /// from an empty history, while a session can resume by supplying the
    /// transcript accumulated so far.
    #[must_use]
    pub fn with_history(mut self, history: Vec<Message>) -> Self {
        self.history = history;
        self
    }

    /// Current conversation history, including all messages appended during a run.
    #[must_use]
    pub fn history(&self) -> &[Message] {
        &self.history
    }

    /// Run the agent loop against `task`, streaming events into `sink`.
    ///
    /// Wraps [`Orchestrator::run_loop`] with the lifecycle [`Hook`] seam:
    /// `on_start` may augment the system prompt before any request, `on_event`
    /// observes every streamed [`AgentEvent`], and `on_end` sees the terminal
    /// [`RunOutcome`]. With no hooks registered every step is a no-op and the run
    /// is byte-for-byte identical to a hook-free orchestrator.
    pub fn run(
        &mut self,
        task: &str,
        cancel: &CancelToken,
        sink: &mut dyn FnMut(AgentEvent),
    ) -> AgentResult<RunOutcome> {
        // Cloning a `Vec<&dyn Hook>` copies references only; this detaches the
        // hook list from `self` so the event-forwarding closure can run without
        // aliasing the `&mut self` that `run_loop` needs.
        let hooks: Vec<&'a dyn Hook> = self.hooks.clone();
        // Hooks augment a per-run *copy* of the prompt; `def` stays the immutable
        // source of truth, so re-running the same orchestrator never compounds a
        // hook's effect (and a no-hook run sends the prompt verbatim).
        let mut system_prompt = self.def.system_prompt.clone();
        for hook in &hooks {
            hook.on_start(&mut system_prompt);
        }
        let outcome = {
            let mut hooked = |event: AgentEvent| {
                for hook in &hooks {
                    hook.on_event(&event);
                }
                sink(event);
            };
            self.run_loop(task, &system_prompt, cancel, &mut hooked)
        }?;
        for hook in &hooks {
            hook.on_end(&outcome);
        }
        Ok(outcome)
    }

    /// Drive provider turns until the model stops, a guardrail trips, or
    /// `def.max_turns` is reached. Honors `cancel` cooperatively between and
    /// within turns. `system_prompt` is the (possibly hook-augmented) prompt for
    /// this run. This is the hook-free core invoked by [`Orchestrator::run`].
    fn run_loop(
        &mut self,
        task: &str,
        system_prompt: &str,
        cancel: &CancelToken,
        sink: &mut dyn FnMut(AgentEvent),
    ) -> AgentResult<RunOutcome> {
        // System is conveyed through `ChatRequest.system` (every provider
        // consumes that channel). Seeding a `Role::System` message here too
        // would double-send the prompt on providers that also map history
        // system messages into the wire (OpenAI `developer` item, xAI `system`
        // message); Anthropic drops them. Append only the new user turn so a
        // host-managed session can seed prior history and continue it.
        self.history.push(Message::user(task));
        let tools = self.filtered_tools();

        let mut usage = Usage::default();
        let mut final_text = String::new();
        let mut consecutive_failures: u32 = 0;
        let mut requests: u32 = 0;
        let mut verified = false;

        for turn in 1..=self.def.max_turns {
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            sink(AgentEvent::TurnStarted(turn));

            let remaining = self.def.max_turns - turn + 1;
            let turn_system = turn_budget_prompt(system_prompt, remaining, self.def.max_turns);
            let response = self.execute_turn(turn_system.as_ref(), &tools, cancel, sink)?;
            requests += 1;
            accumulate_usage(&mut usage, &response.usage);
            sink(AgentEvent::Usage {
                input_tokens: response.usage.input_tokens,
                output_tokens: response.usage.output_tokens,
                cache_read_tokens: response.usage.cache_read_tokens,
                cache_creation_tokens: response.usage.cache_creation_tokens,
            });
            if !response.content.is_empty() {
                final_text = response.content.clone();
            }

            if response.tool_calls.is_empty() && response.finish_reason == FinishReason::Stop {
                if self.def.verify_completion && !verified {
                    // Opt-in: one self-check pass before accepting completion, so the
                    // model can catch a premature stop. Bounded to fire at most once.
                    verified = true;
                    self.history.push(Message::user(COMPLETION_VERIFY_PROMPT));
                    self.compact_history();
                    continue;
                }
                sink(AgentEvent::Done {
                    reason: "completed".to_string(),
                });
                return Ok(RunOutcome {
                    reason: "completed".to_string(),
                    turns: turn,
                    final_text,
                    usage,
                });
            }

            let guard = self.dispatch_tool_calls(
                &response.tool_calls,
                cancel,
                sink,
                &mut consecutive_failures,
            )?;
            if let Some(reason) = guard {
                sink(AgentEvent::Interrupted(reason.clone()));
                return Ok(RunOutcome {
                    reason,
                    turns: turn,
                    final_text,
                    usage,
                });
            }

            if let Some(limit) = self.def.max_requests_per_turn
                && requests >= limit
            {
                let reason = format!("max_requests_per_turn ({limit}) reached");
                sink(AgentEvent::Interrupted(reason.clone()));
                return Ok(RunOutcome {
                    reason,
                    turns: turn,
                    final_text,
                    usage,
                });
            }

            self.compact_history();
        }

        let reason = "max_turns".to_string();
        sink(AgentEvent::Done {
            reason: reason.clone(),
        });
        Ok(RunOutcome {
            reason,
            turns: self.def.max_turns,
            final_text,
            usage,
        })
    }

    /// Bound the history so it stays within the model's context window. The
    /// trigger budget is derived from [`ModelCapabilities`]; see [`compaction`]
    /// for the elision strategy.
    fn compact_history(&mut self) {
        let threshold = compaction::compact_threshold_tokens(&self.caps);
        compaction::compact_history(&mut self.history, threshold);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::ProviderId;
    use crate::message::{ChatDelta, ChatRequest, ChatResponse, Role, ToolCall, ToolSpec};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A provider that replays a fixed script of responses, one per `chat` call,
    /// streaming each response's text as a single delta first.
    struct MockProvider {
        responses: Vec<ChatResponse>,
        calls: AtomicUsize,
    }

    impl MockProvider {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                responses,
                calls: AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl LlmProvider for MockProvider {
        fn id(&self) -> ProviderId {
            ProviderId::Anthropic
        }

        fn chat(
            &self,
            _req: &ChatRequest,
            _cancel: &CancelToken,
            sink: &mut dyn FnMut(ChatDelta),
        ) -> AgentResult<ChatResponse> {
            let idx = self.calls.fetch_add(1, Ordering::SeqCst);
            let response = self
                .responses
                .get(idx)
                .cloned()
                .ok_or_else(|| AgentError::Provider("mock script exhausted".into()))?;
            if !response.content.is_empty() {
                sink(ChatDelta::Text(response.content.clone()));
            }
            Ok(response)
        }
    }

    /// A toolbox advertising one `echo` tool that returns its arguments verbatim.
    struct MockToolBox {
        calls: AtomicUsize,
    }

    impl MockToolBox {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl ToolBox for MockToolBox {
        fn specs(&self) -> Vec<ToolSpec> {
            vec![ToolSpec {
                name: "echo".into(),
                description: "echo back the arguments".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }]
        }

        fn call(
            &self,
            name: &str,
            args: &serde_json::Value,
            _cancel: &CancelToken,
        ) -> AgentResult<serde_json::Value> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if name != "echo" {
                return Err(AgentError::Tool(format!("unknown tool {name}")));
            }
            Ok(args.clone())
        }
    }

    fn tool_call(id: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "echo".into(),
            arguments: args,
        }
    }

    fn response(content: &str, tool_calls: Vec<ToolCall>, finish: FinishReason) -> ChatResponse {
        ChatResponse {
            content: content.into(),
            reasoning: None,
            reasoning_signature: None,
            tool_calls,
            finish_reason: finish,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Usage::default()
            },
        }
    }

    fn def() -> AgentDef {
        AgentDef {
            system_prompt: "be helpful".into(),
            model: "mock-model".into(),
            max_turns: 5,
            ..AgentDef::default()
        }
    }

    #[test]
    fn run_drives_tool_loop_to_completion() {
        let provider = MockProvider::new(vec![
            response(
                "calling tool",
                vec![tool_call("call_1", serde_json::json!({"text": "hi"}))],
                FinishReason::ToolUse,
            ),
            response("all done", Vec::new(), FinishReason::Stop),
        ]);
        let toolbox = MockToolBox::new();
        let mut orch = Orchestrator::new(&provider, &toolbox, def());

        let mut events = Vec::new();
        let outcome = orch
            .run("do the thing", &CancelToken::never(), &mut |event| {
                events.push(event)
            })
            .expect("run should complete");

        // Two provider requests (tool turn + stop turn) and one tool invocation.
        assert_eq!(provider.call_count(), 2);
        assert_eq!(toolbox.call_count(), 1);

        assert_eq!(outcome.reason, "completed");
        assert_eq!(outcome.turns, 2);
        assert_eq!(outcome.final_text, "all done");
        // Usage accumulated across both responses.
        assert_eq!(outcome.usage.input_tokens, 20);
        assert_eq!(outcome.usage.output_tokens, 10);

        // Event ordering across the two turns.
        let kinds: Vec<&str> = events.iter().map(event_kind).collect();
        assert_eq!(
            kinds,
            vec![
                "turn",          // turn 1 started
                "text",          // "calling tool"
                "usage",         // turn 1 token usage
                "tool_started",  // echo
                "tool_finished", // echo ok
                "turn",          // turn 2 started
                "text",          // "all done"
                "usage",         // turn 2 token usage
                "done",          // completed
            ]
        );

        assert!(matches!(
            &events[3],
            AgentEvent::ToolStarted { name, .. } if name == "echo"
        ));
        assert!(matches!(
            &events[4],
            AgentEvent::ToolFinished { ok: true, output, .. } if output.contains("\"text\":\"hi\"")
        ));
        assert!(matches!(
            events.last(),
            Some(AgentEvent::Done { reason }) if reason == "completed"
        ));

        // History: user, assistant(tool_call), tool result, assistant(final).
        // The system prompt is delivered via ChatRequest.system, not seeded as
        // a Role::System history message.
        assert_eq!(orch.history.len(), 4);
        assert_eq!(orch.history[0].role, Role::User);
        assert_eq!(orch.history[1].role, Role::Assistant);
        assert_eq!(orch.history[2].role, Role::Tool);
        assert_eq!(orch.history[2].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn run_stops_after_max_tool_failures() {
        // Every turn asks for a tool call; the toolbox always errors (unknown tool),
        // so consecutive failures climb until the guardrail interrupts the run.
        let failing = response(
            "try a bad tool",
            vec![ToolCall {
                id: "x".into(),
                name: "missing".into(),
                arguments: serde_json::json!({}),
            }],
            FinishReason::ToolUse,
        );
        let provider = MockProvider::new(vec![failing.clone(), failing.clone(), failing]);
        let toolbox = MockToolBox::new();
        let mut agent_def = def();
        agent_def.max_tool_failures = Some(2);
        let mut orch = Orchestrator::new(&provider, &toolbox, agent_def);

        let mut events = Vec::new();
        let outcome = orch
            .run("go", &CancelToken::never(), &mut |event| events.push(event))
            .expect("run should yield an outcome");

        // Third failure (> limit of 2) trips the guardrail on turn 3.
        assert_eq!(outcome.turns, 3);
        assert!(outcome.reason.contains("max_tool_failures"));
        assert!(matches!(
            events.last(),
            Some(AgentEvent::Interrupted(reason)) if reason.contains("max_tool_failures")
        ));
    }

    #[test]
    fn repeated_tool_failures_nudge_the_model_with_remaining_budget() {
        // The toolbox always errors and the model keeps calling. From the 2nd
        // consecutive failure on, the tool result carries a budget nudge so the
        // model can adapt; the first failure passes through untouched.
        let failing = response(
            "try a bad tool",
            vec![ToolCall {
                id: "x".into(),
                name: "missing".into(),
                arguments: serde_json::json!({}),
            }],
            FinishReason::ToolUse,
        );
        let provider = MockProvider::new(vec![failing.clone(), failing.clone(), failing]);
        let toolbox = MockToolBox::new();
        let mut agent_def = def();
        agent_def.max_turns = 3;
        agent_def.max_tool_failures = Some(5); // high enough not to abort within 3 turns
        let mut orch = Orchestrator::new(&provider, &toolbox, agent_def);

        orch.run("go", &CancelToken::never(), &mut |_| {})
            .expect("run should yield an outcome");

        let tool_results: Vec<&str> = orch
            .history
            .iter()
            .filter(|m| m.role == Role::Tool)
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(tool_results.len(), 3);
        // First failure: raw error, no nudge yet.
        assert!(tool_results[0].contains("unknown tool"));
        assert!(!tool_results[0].contains("DIFFERENT approach"));
        // Repeated failures: budget nudge surfaced so the model changes approach.
        assert!(tool_results[1].contains("DIFFERENT approach"));
        assert!(tool_results[1].contains("attempt(s) left"));
    }

    #[test]
    fn verify_completion_runs_one_self_check_before_finishing() {
        // The model signals completion twice; with verify_completion on, the first
        // stop triggers a single self-check pass, so the run takes an extra turn and
        // the verify prompt lands in history.
        let provider = MockProvider::new(vec![
            response("done", Vec::new(), FinishReason::Stop),
            response("really done", Vec::new(), FinishReason::Stop),
        ]);
        let toolbox = MockToolBox::new();
        let mut agent_def = def();
        agent_def.verify_completion = true;
        let mut orch = Orchestrator::new(&provider, &toolbox, agent_def);

        let outcome = orch
            .run("go", &CancelToken::never(), &mut |_| {})
            .expect("run completes");

        assert_eq!(outcome.reason, "completed");
        assert_eq!(outcome.turns, 2); // the self-check added exactly one turn
        assert!(
            orch.history
                .iter()
                .any(|m| m.role == Role::User && m.content.contains("double-check"))
        );
    }

    #[test]
    fn turn_budget_prompt_warns_only_near_the_cap() {
        // Ample budget: prompt unchanged.
        assert_eq!(turn_budget_prompt("sys", 10, 40).as_ref(), "sys");
        // Near the cap: a heads-up is appended.
        let near = turn_budget_prompt("sys", 2, 40);
        assert!(near.starts_with("sys"));
        assert!(near.contains("turn(s) left"));
        // Tiny caps never warn (the whole run is trivially near the cap).
        assert_eq!(turn_budget_prompt("sys", 1, 2).as_ref(), "sys");
    }

    #[test]
    fn tool_filter_narrows_advertised_specs() {
        let provider = MockProvider::new(vec![response("done", Vec::new(), FinishReason::Stop)]);
        let toolbox = MockToolBox::new();
        let mut agent_def = def();
        agent_def.tool_filter = Some(vec!["not_echo".into()]);
        let orch = Orchestrator::new(&provider, &toolbox, agent_def);

        assert!(orch.filtered_tools().is_empty());
    }

    /// A provider that records the system prompt of its first request, then
    /// replies with a single streamed `Stop` response.
    struct CapturingProvider {
        system: std::sync::Mutex<Option<String>>,
    }

    impl CapturingProvider {
        fn new() -> Self {
            Self {
                system: std::sync::Mutex::new(None),
            }
        }

        fn captured(&self) -> Option<String> {
            self.system.lock().expect("system lock").clone()
        }
    }

    impl LlmProvider for CapturingProvider {
        fn id(&self) -> ProviderId {
            ProviderId::Anthropic
        }

        fn chat(
            &self,
            req: &ChatRequest,
            _cancel: &CancelToken,
            sink: &mut dyn FnMut(ChatDelta),
        ) -> AgentResult<ChatResponse> {
            *self.system.lock().expect("system lock") = req.system.clone();
            let reply = response("done", Vec::new(), FinishReason::Stop);
            sink(ChatDelta::Text(reply.content.clone()));
            Ok(reply)
        }
    }

    /// A provider that records each request's message contents.
    struct HistoryProvider {
        requests: std::sync::Mutex<Vec<Vec<Message>>>,
    }

    impl HistoryProvider {
        fn new() -> Self {
            Self {
                requests: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<Vec<Message>> {
            self.requests.lock().expect("requests lock").clone()
        }
    }

    impl LlmProvider for HistoryProvider {
        fn id(&self) -> ProviderId {
            ProviderId::Anthropic
        }

        fn chat(
            &self,
            req: &ChatRequest,
            _cancel: &CancelToken,
            sink: &mut dyn FnMut(ChatDelta),
        ) -> AgentResult<ChatResponse> {
            self.requests
                .lock()
                .expect("requests lock")
                .push(req.messages.clone());
            let reply = response("ack", Vec::new(), FinishReason::Stop);
            sink(ChatDelta::Text(reply.content.clone()));
            Ok(reply)
        }
    }

    /// A hook that augments the system prompt and records the lifecycle it sees.
    struct RecordingHook {
        requests: std::sync::Mutex<Vec<Option<String>>>,
        events: std::sync::Mutex<Vec<String>>,
        ended: std::sync::Mutex<Option<String>>,
    }

    impl RecordingHook {
        fn new() -> Self {
            Self {
                requests: std::sync::Mutex::new(Vec::new()),
                events: std::sync::Mutex::new(Vec::new()),
                ended: std::sync::Mutex::new(None),
            }
        }
    }

    impl Hook for RecordingHook {
        fn on_start(&self, system_prompt: &mut String) {
            system_prompt.push_str(" [augmented]");
        }

        fn on_request(&self, req: &mut ChatRequest) {
            self.requests
                .lock()
                .expect("requests lock")
                .push(req.system.clone());
        }

        fn on_event(&self, event: &AgentEvent) {
            self.events
                .lock()
                .expect("events lock")
                .push(event_kind(event).to_string());
        }

        fn on_end(&self, outcome: &RunOutcome) {
            *self.ended.lock().expect("ended lock") = Some(outcome.reason.clone());
        }
    }

    #[test]
    fn seeded_history_is_preserved_and_extended() {
        let provider = HistoryProvider::new();
        let toolbox = MockToolBox::new();
        let seed = vec![Message::user("first"), Message::assistant("one")];
        let mut orch = Orchestrator::new(&provider, &toolbox, def()).with_history(seed.clone());

        orch.run("second", &CancelToken::never(), &mut |_| {})
            .expect("run should complete");

        let requests = provider.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].len(), 3);
        assert_eq!(requests[0][0].content, "first");
        assert_eq!(requests[0][1].content, "one");
        assert_eq!(requests[0][2].content, "second");
        assert_eq!(orch.history().len(), 4);
        assert_eq!(orch.history()[3].content, "ack");
    }

    #[test]
    fn hooks_augment_prompt_and_observe_lifecycle() {
        let provider = CapturingProvider::new();
        let toolbox = MockToolBox::new();
        let hook = RecordingHook::new();
        let mut orch = Orchestrator::new(&provider, &toolbox, def()).with_hooks(vec![&hook]);

        let mut events = Vec::new();
        let outcome = orch
            .run("go", &CancelToken::never(), &mut |event| events.push(event))
            .expect("run should complete");

        // `on_start` augmented the very prompt the provider received on the wire,
        // while `def` (the static source of truth) is left untouched.
        assert_eq!(
            provider.captured().as_deref(),
            Some("be helpful [augmented]")
        );
        assert_eq!(orch.def.system_prompt, "be helpful");

        // `on_event` observed exactly the events the sink saw, in the same order.
        let sink_kinds: Vec<String> = events.iter().map(|e| event_kind(e).to_string()).collect();
        assert_eq!(*hook.events.lock().expect("events lock"), sink_kinds);

        // `on_end` saw the terminal outcome.
        assert_eq!(
            hook.ended.lock().expect("ended lock").as_deref(),
            Some("completed")
        );
        assert_eq!(outcome.reason, "completed");
    }

    #[test]
    fn run_without_hooks_leaves_system_prompt_untouched() {
        let provider = CapturingProvider::new();
        let toolbox = MockToolBox::new();
        let mut orch = Orchestrator::new(&provider, &toolbox, def());

        orch.run("go", &CancelToken::never(), &mut |_| {})
            .expect("run should complete");

        // With no hooks the configured prompt is sent and stored exactly as-is.
        assert_eq!(provider.captured().as_deref(), Some("be helpful"));
        assert_eq!(orch.def.system_prompt, "be helpful");
    }

    #[test]
    fn hooks_observe_request_once_per_turn() {
        let provider = MockProvider::new(vec![
            response(
                "calling tool",
                vec![tool_call("call_1", serde_json::json!({"text": "hi"}))],
                FinishReason::ToolUse,
            ),
            response("all done", Vec::new(), FinishReason::Stop),
        ]);
        let toolbox = MockToolBox::new();
        let hook = RecordingHook::new();
        let mut orch = Orchestrator::new(&provider, &toolbox, def()).with_hooks(vec![&hook]);

        orch.run("go", &CancelToken::never(), &mut |_| {})
            .expect("run should complete");

        let requests = hook.requests.lock().expect("requests lock");
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .all(|system| system.as_deref() == Some("be helpful [augmented]"))
        );
    }

    #[test]
    fn hooks_are_reapplied_per_run_not_compounded() {
        let provider = CapturingProvider::new();
        let toolbox = MockToolBox::new();
        let hook = RecordingHook::new();
        let mut orch = Orchestrator::new(&provider, &toolbox, def()).with_hooks(vec![&hook]);

        orch.run("first", &CancelToken::never(), &mut |_| {})
            .expect("first run");
        let first = provider.captured();
        orch.run("second", &CancelToken::never(), &mut |_| {})
            .expect("second run");
        let second = provider.captured();

        // Augmentation is applied to a fresh copy each run, never compounded, and
        // `def` is never mutated — the seam is safely re-runnable.
        assert_eq!(first.as_deref(), Some("be helpful [augmented]"));
        assert_eq!(second, first);
        assert_eq!(orch.def.system_prompt, "be helpful");
    }

    /// A provider that fails its first `chat` with a context-overflow error and
    /// then succeeds, recording the message count it saw on the retry.
    struct OverflowOnceProvider {
        calls: AtomicUsize,
        retry_messages: std::sync::Mutex<Option<usize>>,
    }

    impl OverflowOnceProvider {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                retry_messages: std::sync::Mutex::new(None),
            }
        }
    }

    impl LlmProvider for OverflowOnceProvider {
        fn id(&self) -> ProviderId {
            ProviderId::Anthropic
        }

        fn chat(
            &self,
            req: &ChatRequest,
            _cancel: &CancelToken,
            sink: &mut dyn FnMut(ChatDelta),
        ) -> AgentResult<ChatResponse> {
            let idx = self.calls.fetch_add(1, Ordering::SeqCst);
            if idx == 0 {
                return Err(AgentError::Http(
                    "HTTP 400: prompt is too long: 250000 tokens > 200000 maximum".into(),
                ));
            }
            *self.retry_messages.lock().expect("lock") = Some(req.messages.len());
            let reply = response("recovered", Vec::new(), FinishReason::Stop);
            sink(ChatDelta::Text(reply.content.clone()));
            Ok(reply)
        }
    }

    #[test]
    fn context_overflow_truncates_largest_tool_result_and_retries_once() {
        let provider = OverflowOnceProvider::new();
        let toolbox = MockToolBox::new();
        let mut orch = Orchestrator::new(&provider, &toolbox, def());
        // Seed a transcript carrying one very large tool result.
        orch = orch.with_history(vec![
            Message::user("earlier"),
            Message::tool("call_big", "search", "X".repeat(40_000)),
            Message::assistant("noted"),
        ]);

        let outcome = orch
            .run("continue", &CancelToken::never(), &mut |_| {})
            .expect("overflow should be recovered");

        // Two provider calls: the overflow, then the successful retry.
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert_eq!(outcome.reason, "completed");
        assert_eq!(outcome.final_text, "recovered");
        // Exactly one truncation was recorded and the big body was stubbed.
        assert_eq!(orch.truncations(), 1);
        assert!(
            orch.history
                .iter()
                .any(|m| m.role == Role::Tool && m.content == compaction::ELIDED_TOOL_OUTPUT)
        );
        // The retry saw the same message count (truncation stubs in place, does
        // not drop messages).
        assert_eq!(
            *provider.retry_messages.lock().expect("lock"),
            Some(orch.history.len() - 1) // assistant reply appended after the call
        );
    }

    #[test]
    fn non_overflow_errors_are_not_retried() {
        // A 401 must surface immediately, with no truncation and a single call.
        struct AuthFail(AtomicUsize);
        impl LlmProvider for AuthFail {
            fn id(&self) -> ProviderId {
                ProviderId::Anthropic
            }
            fn chat(
                &self,
                _req: &ChatRequest,
                _cancel: &CancelToken,
                _sink: &mut dyn FnMut(ChatDelta),
            ) -> AgentResult<ChatResponse> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err(AgentError::Http("HTTP 401: unauthorized".into()))
            }
        }
        let provider = AuthFail(AtomicUsize::new(0));
        let toolbox = MockToolBox::new();
        let mut orch = Orchestrator::new(&provider, &toolbox, def())
            .with_history(vec![Message::tool("c", "search", "X".repeat(40_000))]);

        let err = orch
            .run("go", &CancelToken::never(), &mut |_| {})
            .expect_err("401 is fatal");
        assert!(matches!(err, AgentError::Http(m) if m.contains("401")));
        assert_eq!(provider.0.load(Ordering::SeqCst), 1);
        assert_eq!(orch.truncations(), 0);
    }

    /// A provider that streams one tool-call delta on its first turn (advisory),
    /// returns the assembled call, then stops.
    struct StreamingToolProvider {
        calls: AtomicUsize,
    }

    impl LlmProvider for StreamingToolProvider {
        fn id(&self) -> ProviderId {
            ProviderId::Anthropic
        }

        fn chat(
            &self,
            _req: &ChatRequest,
            _cancel: &CancelToken,
            sink: &mut dyn FnMut(ChatDelta),
        ) -> AgentResult<ChatResponse> {
            let idx = self.calls.fetch_add(1, Ordering::SeqCst);
            if idx == 0 {
                let call = tool_call("call_1", serde_json::json!({"text": "hi"}));
                // Advisory streamed fragment, then the assembled response.
                sink(ChatDelta::ToolCall(call.clone()));
                return Ok(response("", vec![call], FinishReason::ToolUse));
            }
            let reply = response("done", Vec::new(), FinishReason::Stop);
            sink(ChatDelta::Text(reply.content.clone()));
            Ok(reply)
        }
    }

    #[test]
    fn streamed_tool_call_deltas_are_forwarded_without_changing_dispatch() {
        let provider = StreamingToolProvider {
            calls: AtomicUsize::new(0),
        };
        let toolbox = MockToolBox::new();
        let mut orch = Orchestrator::new(&provider, &toolbox, def());

        let mut events = Vec::new();
        let outcome = orch
            .run("go", &CancelToken::never(), &mut |event| events.push(event))
            .expect("run completes");

        // The advisory delta surfaced exactly once and carries the call shape.
        let deltas: Vec<&AgentEvent> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolCallDelta { .. }))
            .collect();
        assert_eq!(deltas.len(), 1);
        assert!(matches!(
            deltas[0],
            AgentEvent::ToolCallDelta { name, .. } if name == "echo"
        ));
        // Dispatch still ran the assembled call exactly once (not driven by the delta).
        assert_eq!(toolbox.call_count(), 1);
        assert_eq!(outcome.reason, "completed");
    }

    /// A provider that returns reasoning text plus a signature, then stops.
    struct ReasoningProvider;

    impl LlmProvider for ReasoningProvider {
        fn id(&self) -> ProviderId {
            ProviderId::Anthropic
        }

        fn chat(
            &self,
            _req: &ChatRequest,
            _cancel: &CancelToken,
            sink: &mut dyn FnMut(ChatDelta),
        ) -> AgentResult<ChatResponse> {
            sink(ChatDelta::Reasoning("thinking".into()));
            sink(ChatDelta::Text("answer".into()));
            let mut reply = response("answer", Vec::new(), FinishReason::Stop);
            reply.reasoning = Some("thinking".into());
            reply.reasoning_signature = Some("sig-1".into());
            Ok(reply)
        }
    }

    #[test]
    fn assistant_message_round_trips_reasoning_and_signature() {
        let provider = ReasoningProvider;
        let toolbox = MockToolBox::new();
        let mut orch = Orchestrator::new(&provider, &toolbox, def());
        orch.run("go", &CancelToken::never(), &mut |_| {})
            .expect("run completes");

        // The assistant message carries the reasoning + signature for replay.
        let assistant = orch
            .history
            .iter()
            .find(|m| m.role == Role::Assistant)
            .expect("an assistant message");
        let reasoning = assistant.reasoning.as_ref().expect("reasoning stored");
        assert_eq!(reasoning.text, "thinking");
        assert_eq!(reasoning.signature.as_deref(), Some("sig-1"));
    }

    #[test]
    fn resume_seeds_prior_transcript_without_rerunning_tool_calls() {
        // The prior session already called `echo` and recorded its result. The
        // resumed run must see that transcript and NOT re-invoke the tool.
        let provider = HistoryProvider::new();
        let toolbox = MockToolBox::new();
        let mut prior_assistant = Message::assistant("calling echo");
        prior_assistant.tool_calls = vec![tool_call("call_prior", serde_json::json!({"t": 1}))];
        let state = ResumeState {
            history: vec![
                Message::user("earlier task"),
                prior_assistant,
                Message::tool("call_prior", "echo", "prior result"),
            ],
            truncations: 2,
        };
        let mut orch = Orchestrator::resume(&provider, &toolbox, def(), state);

        orch.run("next task", &CancelToken::never(), &mut |_| {})
            .expect("resumed run completes");

        // The prior tool call was NOT re-executed (only what this turn requests).
        assert_eq!(toolbox.call_count(), 0);
        // The provider's first request carried the full prior transcript plus the
        // new user turn (4 messages: 3 seeded + the new "next task").
        let requests = provider.requests();
        assert_eq!(requests[0].len(), 4);
        assert_eq!(requests[0][0].content, "earlier task");
        assert_eq!(requests[0][2].role, Role::Tool);
        assert_eq!(requests[0][2].content, "prior result");
        assert_eq!(requests[0][3].content, "next task");
        // Restored counters carried over.
        assert_eq!(orch.truncations(), 2);
    }

    #[test]
    fn resume_with_default_state_matches_new() {
        // An empty ResumeState behaves exactly like `new` (empty history).
        let provider = HistoryProvider::new();
        let toolbox = MockToolBox::new();
        let mut orch = Orchestrator::resume(&provider, &toolbox, def(), ResumeState::default());
        orch.run("go", &CancelToken::never(), &mut |_| {})
            .expect("run completes");
        let requests = provider.requests();
        assert_eq!(requests[0].len(), 1);
        assert_eq!(requests[0][0].content, "go");
        assert_eq!(orch.truncations(), 0);
    }

    fn event_kind(event: &AgentEvent) -> &'static str {
        match event {
            AgentEvent::TurnStarted(_) => "turn",
            AgentEvent::AssistantText(_) => "text",
            AgentEvent::Reasoning(_) => "reasoning",
            AgentEvent::ToolStarted { .. } => "tool_started",
            AgentEvent::ToolFinished { .. } => "tool_finished",
            AgentEvent::Interrupted(_) => "interrupted",
            AgentEvent::Usage { .. } => "usage",
            AgentEvent::ToolCallDelta { .. } => "tool_call_delta",
            AgentEvent::Done { .. } => "done",
        }
    }
}
