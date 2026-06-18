use super::{API_IMAGE_MODEL, DEFAULT_CODEX_CHAT_MODEL};
use crate::openai::util::*;
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn image_payload(args: &Value, prompt: &str) -> Result<Value> {
    Ok(json!({
        "model": DEFAULT_CODEX_CHAT_MODEL,
        "store": false,
        "instructions": "Generate the requested image.",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": prompt }]
        }],
        "tools": [{
            "type": "image_generation",
            "model": API_IMAGE_MODEL,
            "size": image_size(args)?,
            "quality": image_quality(args)?,
            "output_format": "png",
            "background": "opaque",
            "partial_images": 1
        }],
        "tool_choice": {
            "type": "allowed_tools",
            "mode": "required",
            "tools": [{ "type": "image_generation" }]
        },
        "stream": true
    }))
}

pub(super) fn image_size(args: &Value) -> Result<&'static str> {
    match optional_string(args, "size").as_deref().unwrap_or("square") {
        "landscape" => Ok("1536x1024"),
        "square" => Ok("1024x1024"),
        "portrait" => Ok("1024x1536"),
        other => {
            bail!("unsupported OpenAI image size `{other}`; use landscape, square, or portrait")
        }
    }
}

fn image_quality(args: &Value) -> Result<String> {
    let quality = string_arg(args, "quality", "high");
    match quality.as_str() {
        "high" | "auto" => Ok(quality),
        other => bail!("unsupported OpenAI image quality `{other}`; use high or auto"),
    }
}

pub(super) fn save_image_sse(sse: &str, output_path: &Path) -> Result<()> {
    let b64 = extract_latest_image_base64_from_sse(sse)?
        .ok_or_else(|| anyhow!("OpenAI image stream did not include image base64"))?;
    let bytes = STANDARD
        .decode(b64.as_bytes())
        .context("invalid OpenAI image base64")?;
    write_bytes(output_path, &bytes)
}

pub(super) fn extract_latest_image_base64_from_sse(sse: &str) -> Result<Option<String>> {
    let mut latest = None;
    for line in sse.lines() {
        let Some(data) = line.trim().strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            break;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(message) = sse_error_message(&value) {
            bail!("OpenAI image generation failed: {message}");
        }
        latest_nested_image_base64(&value, &mut latest);
        if value.get("type").and_then(Value::as_str) == Some("response.completed") {
            break;
        }
    }
    Ok(latest)
}

fn sse_error_message(value: &Value) -> Option<String> {
    match value.get("type").and_then(Value::as_str) {
        Some("error" | "response.failed" | "response.incomplete") => {}
        _ => return None,
    }
    value
        .pointer("/error/message")
        .or_else(|| value.pointer("/response/error/message"))
        .or_else(|| value.pointer("/response/incomplete_details/reason"))
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| Some(value.to_string()))
}

fn latest_nested_image_base64(value: &Value, latest: &mut Option<String>) {
    match value {
        Value::Object(object) => {
            if let Some(b64) = object.get("partial_image_b64").and_then(Value::as_str) {
                *latest = Some(b64.to_string());
            }
            if object.get("type").and_then(Value::as_str) == Some("image_generation_call")
                && let Some(b64) = object.get("result").and_then(Value::as_str)
            {
                *latest = Some(b64.to_string());
            }
            for child in object.values() {
                latest_nested_image_base64(child, latest);
            }
        }
        Value::Array(items) => {
            for item in items {
                latest_nested_image_base64(item, latest);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_image_size_aliases() {
        assert_eq!(image_size(&json!({})).unwrap(), "1024x1024");
        assert_eq!(
            image_size(&json!({ "size": "square" })).unwrap(),
            "1024x1024"
        );
        assert_eq!(
            image_size(&json!({ "size": "landscape" })).unwrap(),
            "1536x1024"
        );
        assert_eq!(
            image_size(&json!({ "size": "portrait" })).unwrap(),
            "1024x1536"
        );
        assert!(image_size(&json!({ "size": "wide" })).is_err());
    }

    #[test]
    fn image_payload_matches_codex_wire_contract() {
        let payload = image_payload(
            &json!({ "size": "portrait", "quality": "auto" }),
            "paint a fox",
        )
        .unwrap();
        assert_eq!(payload["model"], json!(DEFAULT_CODEX_CHAT_MODEL));
        assert_eq!(payload["store"], json!(false));
        assert_eq!(payload["input"][0]["type"], json!("message"));
        assert_eq!(payload["input"][0]["role"], json!("user"));
        assert_eq!(
            payload["input"][0]["content"][0]["type"],
            json!("input_text")
        );
        assert_eq!(
            payload["input"][0]["content"][0]["text"],
            json!("paint a fox")
        );
        assert_eq!(payload["tools"][0]["type"], json!("image_generation"));
        assert_eq!(payload["tools"][0]["model"], json!(API_IMAGE_MODEL));
        assert_eq!(payload["tools"][0]["size"], json!("1024x1536"));
        assert_eq!(payload["tools"][0]["quality"], json!("auto"));
        assert_eq!(payload["tools"][0]["output_format"], json!("png"));
        assert_eq!(payload["tools"][0]["background"], json!("opaque"));
        assert_eq!(payload["tools"][0]["partial_images"], json!(1));
        assert_eq!(payload["tool_choice"]["type"], json!("allowed_tools"));
        assert_eq!(payload["tool_choice"]["mode"], json!("required"));
        assert_eq!(
            payload["tool_choice"]["tools"][0]["type"],
            json!("image_generation")
        );
        assert_eq!(payload["stream"], json!(true));
    }

    #[test]
    fn extracts_latest_base64_from_synthetic_sse() {
        let partial = STANDARD.encode(b"partial");
        let final_b64 = STANDARD.encode(b"final-png");
        let sse = format!(
            "event: response.output_item.added\n\
             data: {{\"item\":{{\"partial_image_b64\":\"{partial}\"}}}}\n\n\
             data: {{\"output\":[{{\"type\":\"image_generation_call\",\"result\":\"{final_b64}\"}}]}}\n\n\
             data: [DONE]\n"
        );
        assert_eq!(
            extract_latest_image_base64_from_sse(&sse).unwrap(),
            Some(final_b64)
        );
    }

    #[test]
    fn failed_sse_event_prevents_partial_success() {
        let partial = STANDARD.encode(b"partial");
        let sse = format!(
            "data: {{\"item\":{{\"partial_image_b64\":\"{partial}\"}}}}\n\n\
             data: {{\"type\":\"response.failed\",\"response\":{{\"error\":{{\"message\":\"boom\"}}}}}}\n"
        );
        let error = extract_latest_image_base64_from_sse(&sse)
            .expect_err("failed event should error despite prior partial image");
        assert!(error.to_string().contains("boom"));
    }
}
