use serde_json::Value;
use std::collections::HashSet;

pub(crate) fn core_tool_specs() -> Vec<Value> {
    ctx_core::tool_specs()
        .as_array()
        .cloned()
        .unwrap_or_default()
}

pub(crate) fn push_unique_tool_specs(
    tools: &mut Vec<Value>,
    names: &mut HashSet<String>,
    specs: Vec<Value>,
) {
    for spec in specs {
        let Some(name) = spec.get("name").and_then(Value::as_str) else {
            tools.push(spec);
            continue;
        };
        if names.insert(name.to_string()) {
            tools.push(spec);
        }
    }
}
