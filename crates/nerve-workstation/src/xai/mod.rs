use anyhow::Result;
use nerve_core::WorkspaceRegistry;
use serde_json::Value;

const DEFAULT_CHAT_MODEL: &str = "grok-build-0.1";
const DEFAULT_X_SEARCH_MODEL: &str = "grok-4.20-0309-reasoning";
const DEFAULT_WEB_SEARCH_MODEL: &str = "grok-build-0.1";
const DEFAULT_IMAGE_MODEL: &str = "grok-imagine-image";
const DEFAULT_VIDEO_MODEL: &str = "grok-imagine-video";
const DEFAULT_IMAGE_TO_VIDEO_MODEL: &str = "grok-imagine-video-1.5";
const DEFAULT_TTS_VOICE: &str = "eve";
const DEFAULT_TTS_LANGUAGE: &str = "en";

mod catalog;
mod http;
mod media;
mod specs;
mod tools;
mod util;

#[must_use]
pub(crate) fn tool_specs() -> Vec<Value> {
    specs::tool_specs()
}

pub(crate) fn handle_tool_call(
    registry: &WorkspaceRegistry,
    params: &Value,
) -> Result<Option<Value>> {
    tools::handle_tool_call(registry, params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn lists_xai_tools() {
        let tools = tool_specs();
        let names: Vec<_> = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect();
        assert!(names.contains(&"xai_responses"));
        assert!(names.contains(&"x_search"));
        assert!(names.contains(&"xai_x_search"));
        assert!(names.contains(&"web_search"));
        assert!(names.contains(&"xai_web_search"));
        assert!(names.contains(&"xai_image_generate"));
    }

    #[test]
    fn unknown_tool_is_not_claimed() {
        let registry = WorkspaceRegistry::new();
        let result = handle_tool_call(
            &registry,
            &json!({ "name": "file_search", "arguments": {} }),
        )
        .expect("dispatch");
        assert!(result.is_none());
    }
}
