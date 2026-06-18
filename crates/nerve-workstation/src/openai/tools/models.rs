use super::*;

pub(super) fn openai_models(_arguments: &Value) -> Result<Value> {
    let creds = auth::resolve_openai_codex_credentials(false)?;
    let url = format!("{}/models?client_version=1.0.0", creds.base_url);
    let live = http_get_json(&url, &creds, Duration::from_secs(30)).unwrap_or(Value::Null);
    let models = crate::openai::catalog::merge_with_live(&live);
    let count = models.len();
    Ok(tool_response(
        json!({
            "provider": "openai-codex-oauth",
            "base_url": creds.base_url,
            "models": models,
        }),
        format!("OpenAI Codex models: {count} known (curated catalog merged with live /models)"),
    ))
}
