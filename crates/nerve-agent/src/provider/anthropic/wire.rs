//! Translation from the provider-neutral [`ChatRequest`] into the Anthropic
//! Messages API request body, plus header construction for both auth modes.
//!
//! Mirrors the body/headers built by the reference `ClaudeSession` /
//! `NativeClaudeSession` in `GenericAgent/llmcore.py`.

use serde_json::{Map, Value, json};

use crate::auth::{AuthMode, Credential};
use crate::message::{ChatRequest, Message, Role};

/// Pinned Anthropic API version header value.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Beta enabling prompt caching for both auth modes.
pub const PROMPT_CACHING_BETA: &str = "prompt-caching-2024-07-31";
/// Beta required for subscription OAuth (Bearer) auth against the Messages API.
pub const OAUTH_BETA: &str = "oauth-2025-04-20";
/// Impersonation system prompt required for subscription OAuth; rejected without it.
pub const CLAUDE_CODE_SYSTEM: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
/// Default ceiling on generated tokens when the request leaves it unset.
const DEFAULT_MAX_TOKENS: u32 = 8192;

/// HTTP headers for one Messages request, derived from the credential's auth mode.
///
/// `ApiKey` mode uses `x-api-key`; `Oauth` mode uses `authorization: Bearer …`
/// plus the OAuth beta and the direct-browser-access flag.
pub fn build_headers(cred: &Credential) -> Vec<(String, String)> {
    let mut headers = vec![
        (
            "anthropic-version".to_string(),
            ANTHROPIC_VERSION.to_string(),
        ),
        ("Content-Type".to_string(), "application/json".to_string()),
    ];
    match cred.mode {
        AuthMode::ApiKey => {
            headers.push(("x-api-key".to_string(), cred.access_token.clone()));
            headers.push((
                "anthropic-beta".to_string(),
                PROMPT_CACHING_BETA.to_string(),
            ));
        }
        AuthMode::Oauth => {
            headers.push((
                "authorization".to_string(),
                format!("Bearer {}", cred.access_token),
            ));
            headers.push((
                "anthropic-beta".to_string(),
                format!("{OAUTH_BETA},{PROMPT_CACHING_BETA}"),
            ));
            headers.push((
                "anthropic-dangerous-direct-browser-access".to_string(),
                "true".to_string(),
            ));
        }
    }
    headers
}

/// Build the full JSON request body for the Messages API (`stream: true`).
pub fn build_body(req: &ChatRequest, cred: &Credential) -> Value {
    let mut body = Map::new();
    body.insert("model".to_string(), json!(req.model));
    body.insert("stream".to_string(), json!(true));
    body.insert(
        "max_tokens".to_string(),
        json!(req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
    );
    if let Some(temp) = req.temperature {
        body.insert("temperature".to_string(), json!(temp));
    }
    if let Some(thinking) = thinking_block(req.reasoning_effort.as_deref()) {
        body.insert("thinking".to_string(), thinking);
    }

    let system = build_system(req.system.as_deref(), cred.mode);
    if !system.is_empty() {
        body.insert("system".to_string(), Value::Array(system));
    }

    let messages = build_messages(&req.messages);
    body.insert("messages".to_string(), Value::Array(messages));

    let tools = build_tools(req);
    if !tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(tools));
    }
    Value::Object(body)
}

/// Map a reasoning-effort hint onto a `thinking` budget block, if recognized.
///
/// The Anthropic API takes an explicit token budget; we map the coarse effort
/// hints onto representative budgets and leave `thinking` unset otherwise.
fn thinking_block(effort: Option<&str>) -> Option<Value> {
    let budget = match effort?.trim().to_ascii_lowercase().as_str() {
        "minimal" | "low" => 2_048,
        "medium" => 8_192,
        "high" => 16_384,
        "xhigh" | "max" => 32_768,
        _ => return None,
    };
    Some(json!({ "type": "enabled", "budget_tokens": budget }))
}

/// Build the `system` text-block array.
///
/// For OAuth the impersonation block is prepended as the first entry; the final
/// block carries an ephemeral `cache_control` marker for prompt caching.
fn build_system(system: Option<&str>, mode: AuthMode) -> Vec<Value> {
    let mut blocks: Vec<Value> = Vec::new();
    if mode == AuthMode::Oauth {
        blocks.push(json!({ "type": "text", "text": CLAUDE_CODE_SYSTEM }));
    }
    if let Some(text) = system.filter(|s| !s.is_empty()) {
        blocks.push(json!({ "type": "text", "text": text }));
    }
    set_last_cache_control(&mut blocks);
    blocks
}

/// Build the `tools` array as `[{name, description, input_schema}]`.
///
/// The last tool carries an ephemeral `cache_control` marker.
fn build_tools(req: &ChatRequest) -> Vec<Value> {
    let mut tools: Vec<Value> = req
        .tools
        .iter()
        .map(|spec| {
            json!({
                "name": spec.name,
                "description": spec.description,
                "input_schema": spec.input_schema,
            })
        })
        .collect();
    set_last_cache_control(&mut tools);
    tools
}

/// Translate neutral [`Message`]s into Anthropic messages.
///
/// `Tool` results become `user` messages with a `tool_result` content block;
/// assistant `tool_calls` become `tool_use` blocks. Consecutive same-role
/// messages are merged so the conversation strictly alternates user/assistant.
/// The last content block of the final message gets an ephemeral cache marker.
fn build_messages(messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<(String, Vec<Value>)> = Vec::new();
    for msg in messages {
        let Some((role, blocks)) = neutral_to_blocks(msg) else {
            continue;
        };
        match out.last_mut() {
            Some((prev_role, prev_blocks)) if *prev_role == role => {
                prev_blocks.extend(blocks);
            }
            _ => out.push((role, blocks)),
        }
    }
    if let Some((_, blocks)) = out.last_mut() {
        set_last_cache_control(blocks);
    }
    out.into_iter()
        .map(|(role, content)| json!({ "role": role, "content": Value::Array(content) }))
        .collect()
}

/// Convert one neutral message into an `(anthropic_role, content_blocks)` pair.
fn neutral_to_blocks(msg: &Message) -> Option<(String, Vec<Value>)> {
    match msg.role {
        Role::System => None,
        Role::User => Some(("user".to_string(), vec![text_block(&msg.content)])),
        Role::Tool => {
            let id = msg.tool_call_id.clone().unwrap_or_default();
            let block = json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": msg.content,
            });
            Some(("user".to_string(), vec![block]))
        }
        Role::Assistant => Some(("assistant".to_string(), assistant_blocks(msg))),
    }
}

/// Build the content blocks for an assistant message: a replayed `thinking`
/// block (when present) first, then optional text, then any `tool_use` blocks
/// reconstructed from its `tool_calls`.
///
/// Anthropic requires the thinking block to lead the turn and carry the original
/// `signature` verbatim; a block without a signature is omitted (replaying
/// unsigned thinking is rejected).
fn assistant_blocks(msg: &Message) -> Vec<Value> {
    let mut blocks: Vec<Value> = Vec::new();
    if let Some(thinking) = thinking_replay_block(msg) {
        blocks.push(thinking);
    }
    if !msg.content.is_empty() {
        blocks.push(text_block(&msg.content));
    }
    for call in &msg.tool_calls {
        blocks.push(json!({
            "type": "tool_use",
            "id": call.id,
            "name": call.name,
            "input": call.arguments,
        }));
    }
    if blocks.is_empty() {
        blocks.push(text_block(""));
    }
    blocks
}

/// A `thinking` block replaying the message's prior reasoning, or `None` when
/// there is no reasoning or it lacks the signature Anthropic requires verbatim.
fn thinking_replay_block(msg: &Message) -> Option<Value> {
    let reasoning = msg.reasoning.as_ref()?;
    let signature = reasoning.signature.as_deref()?;
    Some(json!({
        "type": "thinking",
        "thinking": reasoning.text,
        "signature": signature,
    }))
}

/// A `text` content block.
fn text_block(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

/// Stamp an ephemeral `cache_control` marker on the last block of `blocks`.
fn set_last_cache_control(blocks: &mut [Value]) {
    if let Some(Value::Object(obj)) = blocks.last_mut() {
        obj.insert("cache_control".to_string(), json!({ "type": "ephemeral" }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::ProviderId;
    use crate::message::{ToolCall, ToolSpec};

    fn cred(mode: AuthMode) -> Credential {
        Credential {
            provider: ProviderId::Anthropic,
            mode,
            access_token: "tok".to_string(),
            refresh_token: None,
            expires_at_unix: None,
            account_id: None,
            base_url: "https://api.anthropic.com".to_string(),
        }
    }

    fn base_req() -> ChatRequest {
        ChatRequest {
            model: "claude-x".to_string(),
            system: Some("be brief".to_string()),
            messages: vec![Message::user("hi")],
            tools: Vec::new(),
            temperature: Some(0.5),
            max_tokens: Some(256),
            reasoning_effort: None,
        }
    }

    #[test]
    fn api_key_headers_use_x_api_key() {
        let headers = build_headers(&cred(AuthMode::ApiKey));
        assert!(headers.iter().any(|(k, v)| k == "x-api-key" && v == "tok"));
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "anthropic-version" && v == ANTHROPIC_VERSION)
        );
        assert!(!headers.iter().any(|(k, _)| k == "authorization"));
    }

    #[test]
    fn oauth_headers_use_bearer_and_betas() {
        let headers = build_headers(&cred(AuthMode::Oauth));
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "authorization" && v == "Bearer tok")
        );
        let beta = headers.iter().find(|(k, _)| k == "anthropic-beta").unwrap();
        assert!(beta.1.contains(OAUTH_BETA));
        assert!(beta.1.contains(PROMPT_CACHING_BETA));
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "anthropic-dangerous-direct-browser-access" && v == "true")
        );
    }

    #[test]
    fn oauth_prepends_impersonation_system_block() {
        let body = build_body(&base_req(), &cred(AuthMode::Oauth));
        let system = body["system"].as_array().unwrap();
        assert_eq!(system[0]["text"], CLAUDE_CODE_SYSTEM);
        assert_eq!(system[1]["text"], "be brief");
        assert_eq!(system.last().unwrap()["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn api_key_omits_impersonation_block() {
        let body = build_body(&base_req(), &cred(AuthMode::ApiKey));
        let system = body["system"].as_array().unwrap();
        assert_eq!(system.len(), 1);
        assert_eq!(system[0]["text"], "be brief");
    }

    #[test]
    fn body_carries_stream_and_sampling() {
        let body = build_body(&base_req(), &cred(AuthMode::ApiKey));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["max_tokens"], json!(256));
        assert_eq!(body["temperature"], json!(0.5));
        assert_eq!(body["model"], json!("claude-x"));
    }

    #[test]
    fn missing_max_tokens_falls_back_to_default() {
        let mut req = base_req();
        req.max_tokens = None;
        let body = build_body(&req, &cred(AuthMode::ApiKey));
        assert_eq!(body["max_tokens"], json!(DEFAULT_MAX_TOKENS));
    }

    #[test]
    fn reasoning_effort_maps_to_thinking_budget() {
        let mut req = base_req();
        req.reasoning_effort = Some("high".to_string());
        let body = build_body(&req, &cred(AuthMode::ApiKey));
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], json!(16_384));
    }

    #[test]
    fn unknown_reasoning_effort_omits_thinking() {
        let mut req = base_req();
        req.reasoning_effort = Some("bogus".to_string());
        let body = build_body(&req, &cred(AuthMode::ApiKey));
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn tool_message_becomes_user_tool_result() {
        let mut req = base_req();
        req.messages = vec![
            Message::user("call it"),
            {
                let mut m = Message::assistant("");
                m.tool_calls = vec![ToolCall {
                    id: "tc_1".to_string(),
                    name: "search".to_string(),
                    arguments: json!({"q": "x"}),
                }];
                m
            },
            Message::tool("tc_1", "search", "the result"),
        ];
        let body = build_body(&req, &cred(AuthMode::ApiKey));
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
        let tu = &msgs[1]["content"][0];
        assert_eq!(tu["type"], "tool_use");
        assert_eq!(tu["id"], "tc_1");
        assert_eq!(tu["input"], json!({"q": "x"}));
        assert_eq!(msgs[2]["role"], "user");
        let tr = &msgs[2]["content"][0];
        assert_eq!(tr["type"], "tool_result");
        assert_eq!(tr["tool_use_id"], "tc_1");
        assert_eq!(tr["content"], "the result");
    }

    #[test]
    fn assistant_reasoning_replays_thinking_block_first_with_signature() {
        use crate::message::Reasoning;
        let mut req = base_req();
        let mut assistant = Message::assistant("the answer");
        assistant.reasoning = Some(Reasoning {
            text: "let me think".to_string(),
            signature: Some("sig-xyz".to_string()),
        });
        req.messages = vec![Message::user("q"), assistant];
        let body = build_body(&req, &cred(AuthMode::ApiKey));
        let msgs = body["messages"].as_array().unwrap();
        let blocks = msgs[1]["content"].as_array().unwrap();
        // Thinking block leads, carrying the verbatim signature, then the text.
        assert_eq!(blocks[0]["type"], "thinking");
        assert_eq!(blocks[0]["thinking"], "let me think");
        assert_eq!(blocks[0]["signature"], "sig-xyz");
        assert_eq!(blocks[1]["type"], "text");
        assert_eq!(blocks[1]["text"], "the answer");
    }

    #[test]
    fn assistant_reasoning_without_signature_is_not_replayed() {
        use crate::message::Reasoning;
        let mut req = base_req();
        let mut assistant = Message::assistant("answer");
        assistant.reasoning = Some(Reasoning {
            text: "unsigned thought".to_string(),
            signature: None,
        });
        req.messages = vec![Message::user("q"), assistant];
        let body = build_body(&req, &cred(AuthMode::ApiKey));
        let blocks = body["messages"][1]["content"].as_array().unwrap();
        // No thinking block: an unsigned replay would be rejected by Anthropic.
        assert!(blocks.iter().all(|b| b["type"] != "thinking"));
        assert_eq!(blocks[0]["type"], "text");
    }

    #[test]
    fn consecutive_same_role_messages_merge() {
        let mut req = base_req();
        req.messages = vec![Message::user("a"), Message::user("b")];
        let body = build_body(&req, &cred(AuthMode::ApiKey));
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn tools_translate_to_input_schema_with_cache_marker() {
        let mut req = base_req();
        req.tools = vec![
            ToolSpec {
                name: "a".to_string(),
                description: "first".to_string(),
                input_schema: json!({"type": "object"}),
            },
            ToolSpec {
                name: "b".to_string(),
                description: "second".to_string(),
                input_schema: json!({"type": "object"}),
            },
        ];
        let body = build_body(&req, &cred(AuthMode::ApiKey));
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools[0]["name"], "a");
        assert_eq!(tools[0]["input_schema"], json!({"type": "object"}));
        assert!(tools[0].get("cache_control").is_none());
        assert_eq!(tools.last().unwrap()["cache_control"]["type"], "ephemeral");
    }
}
