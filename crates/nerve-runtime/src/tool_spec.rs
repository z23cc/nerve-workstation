use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

/// Runtime-visible tool specification shape shared with frontend clients.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeToolSpec {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

pub(crate) fn core_tool_specs() -> Vec<Value> {
    nerve_core::tool_specs()
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
