use super::*;

pub(super) fn xai_models(_arguments: &Value) -> Result<Value> {
    let creds = auth::resolve_runtime_credentials(false)?;
    let url = format!("{}/models", creds.base_url);
    let body = http_get_json(&url, &creds.access_token, Duration::from_secs(30))?;
    Ok(tool_response(
        json!({ "provider": "xai-oauth", "base_url": creds.base_url, "models": body }),
        "xAI models fetched".to_string(),
    ))
}
