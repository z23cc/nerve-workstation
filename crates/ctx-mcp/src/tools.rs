use crate::xai;
use ctx_core::{
    WorkspaceRegistry, handle_tool_call_json_with_resolver, tool_specs as core_tool_specs,
};
use serde_json::Value;

pub(crate) fn tool_specs() -> Value {
    let mut tools = core_tool_specs().as_array().cloned().unwrap_or_default();
    tools.extend(xai::tool_specs());
    Value::Array(tools)
}

pub(crate) fn handle_tool_call(
    registry: &WorkspaceRegistry,
    params: &Value,
) -> std::result::Result<Value, String> {
    match xai::handle_tool_call(registry, params) {
        Ok(Some(value)) => return Ok(value),
        Ok(None) => {}
        Err(err) => return Err(err.to_string()),
    }

    serde_json::to_string(params)
        .map_err(|err| err.to_string())
        .and_then(|request| {
            handle_tool_call_json_with_resolver(registry, &request).map_err(|err| err.to_string())
        })
        .and_then(|response| serde_json::from_str(&response).map_err(|err| err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_specs_include_core_and_xai_tools() {
        let specs = tool_specs();
        let names: Vec<_> = specs
            .as_array()
            .expect("tool specs array")
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect();
        assert!(names.contains(&"file_search"));
        assert!(names.contains(&"xai_responses"));
        assert!(names.contains(&"xai_x_search"));
    }
}
