use super::*;
use crate::xai::{DEFAULT_WEB_SEARCH_MODEL, DEFAULT_X_SEARCH_MODEL};

pub(super) fn xai_x_search(arguments: &Value) -> Result<Value> {
    let query = required_string(arguments, "query")?;
    validate_date_range(
        optional_string(arguments, "from_date"),
        optional_string(arguments, "to_date"),
    )?;
    let creds = auth::resolve_runtime_credentials(false)?;
    let mut tool_def = json!({ "type": "x_search" });
    add_x_handle_filters(arguments, &mut tool_def)?;
    add_optional_string(arguments, &mut tool_def, "from_date");
    add_optional_string(arguments, &mut tool_def, "to_date");
    add_optional_bool(arguments, &mut tool_def, "enable_image_understanding");
    add_optional_bool(arguments, &mut tool_def, "enable_video_understanding");
    let model = string_arg(arguments, "model", DEFAULT_X_SEARCH_MODEL);
    let payload = json!({
        "model": model,
        "input": [{ "role": "user", "content": query }],
        "tools": [tool_def],
        "store": false,
    });
    let body = http_post_json(
        &format!("{}/responses", creds.base_url),
        &creds.access_token,
        &payload,
        Duration::from_secs(timeout_arg(arguments, "timeout_seconds", 180)),
    )?;
    let text = extract_response_text(&body).unwrap_or_default();
    let citations = extract_citations(&body);
    Ok(tool_response(
        json!({
            "provider": "xai-oauth",
            "base_url": creds.base_url,
            "model": model,
            "answer": text,
            "citations": citations,
            "raw": body,
        }),
        text_or_summary(&text, "xAI X search completed"),
    ))
}

pub(super) fn xai_web_search(arguments: &Value) -> Result<Value> {
    let query = required_string(arguments, "query")?;
    let limit = bounded_usize(arguments, "limit", 5, 1, 100);
    let creds = auth::resolve_runtime_credentials(false)?;
    let mut web_tool = json!({ "type": "web_search" });
    add_domain_filters(arguments, &mut web_tool)?;
    let prompt = format!(
        "Search the web for this query and return up to {limit} concise results as JSON with fields title, url, description, position. Query: {query}"
    );
    let model = string_arg(arguments, "model", DEFAULT_WEB_SEARCH_MODEL);
    let payload = json!({
        "model": model,
        "input": [{ "role": "user", "content": prompt }],
        "tools": [web_tool],
        "include": ["no_inline_citations"],
    });
    let body = http_post_json(
        &format!("{}/responses", creds.base_url),
        &creds.access_token,
        &payload,
        Duration::from_secs(timeout_arg(arguments, "timeout_seconds", 90)),
    )?;
    let text = extract_response_text(&body).unwrap_or_default();
    Ok(tool_response(
        json!({
            "provider": "xai-oauth",
            "base_url": creds.base_url,
            "model": model,
            "answer": text,
            "citations": extract_citations(&body),
            "raw": body,
        }),
        text_or_summary(&text, "xAI web search completed"),
    ))
}
