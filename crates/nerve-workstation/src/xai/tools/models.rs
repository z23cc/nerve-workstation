use super::*;

pub(super) fn xai_models(_arguments: &Value) -> Result<Value> {
    let creds = auth::resolve_runtime_credentials(false)?;
    let url = format!("{}/models", creds.base_url);
    // `/v1/models` under-reports the subscription (omits grok-4-fast, Composer,
    // …), so merge the live list with the curated catalog. A failed GET degrades
    // to the curated list rather than erroring.
    let live =
        http_get_json(&url, &creds.access_token, Duration::from_secs(30)).unwrap_or(Value::Null);
    let models = crate::xai::catalog::merge_with_live(&live);
    let count = models.len();
    Ok(tool_response(
        json!({ "provider": "xai-oauth", "base_url": creds.base_url, "models": models }),
        format!("xAI models: {count} known (curated catalog merged with live /v1/models)"),
    ))
}
