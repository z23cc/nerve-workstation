use super::*;
use crate::xai::DEFAULT_CHAT_MODEL;

pub(super) fn xai_responses(arguments: &Value) -> Result<Value> {
    let creds = auth::resolve_runtime_credentials(false)?;
    let mut payload = object_payload(arguments)?;
    payload
        .as_object_mut()
        .expect("object")
        .entry("model".to_string())
        .or_insert_with(|| json!(DEFAULT_CHAT_MODEL));
    let timeout = timeout_arg(arguments, "timeout_seconds", 180);
    let body = http_post_json(
        &format!("{}/responses", creds.base_url),
        &creds.access_token,
        &payload,
        Duration::from_secs(timeout),
    )?;
    let text = extract_response_text(&body).unwrap_or_else(|| "xAI response returned".to_string());
    Ok(tool_response(
        json!({ "provider": "xai-oauth", "base_url": creds.base_url, "response": body }),
        text,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_payload_strips_mcp_local_arguments() {
        let payload = object_payload(&json!({
            "model": "grok-test",
            "input": "hello",
            "timeout_seconds": 1,
            "workspace": "default",
        }))
        .expect("payload");
        assert_eq!(payload["model"], json!("grok-test"));
        assert_eq!(payload["input"], json!("hello"));
        assert!(payload.get("timeout_seconds").is_none());
        assert!(payload.get("workspace").is_none());
    }
}
