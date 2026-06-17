use crate::protocol::{
    RUNTIME_EVENT_METHOD, RUNTIME_INFO_METHOD, RUNTIME_JOB_CANCEL_METHOD, RUNTIME_JOB_GET_METHOD,
    RUNTIME_JOB_LIST_METHOD, RUNTIME_JOB_METHODS, RUNTIME_JOB_START_METHOD, RUNTIME_PROTOCOL_NAME,
    RUNTIME_PROTOCOL_VERSION, RUNTIME_TOOLS_LIST_METHOD, RuntimeProtocolSchema,
};
use schemars::schema_for;
use serde_json::Value;

pub const SCHEMA_PATH: &str = "docs/protocol/runtime-v3.schema.json";
pub const CONSTANTS_PATH: &str = "docs/protocol/runtime-v3.constants.json";

#[must_use]
pub fn schema_json() -> String {
    let schema = schema_for!(RuntimeProtocolSchema);
    let mut value = serde_json::to_value(&schema).expect("protocol schema json");
    normalize_schema(&mut value);
    format_json(&value)
}

#[must_use]
pub fn constants_json() -> String {
    format_json(&serde_json::json!({
        "RUNTIME_PROTOCOL_NAME": RUNTIME_PROTOCOL_NAME,
        "RUNTIME_PROTOCOL_VERSION": RUNTIME_PROTOCOL_VERSION,
        "RUNTIME_EVENT_METHOD": RUNTIME_EVENT_METHOD,
        "RUNTIME_INFO_METHOD": RUNTIME_INFO_METHOD,
        "RUNTIME_TOOLS_LIST_METHOD": RUNTIME_TOOLS_LIST_METHOD,
        "RUNTIME_JOB_START_METHOD": RUNTIME_JOB_START_METHOD,
        "RUNTIME_JOB_GET_METHOD": RUNTIME_JOB_GET_METHOD,
        "RUNTIME_JOB_LIST_METHOD": RUNTIME_JOB_LIST_METHOD,
        "RUNTIME_JOB_CANCEL_METHOD": RUNTIME_JOB_CANCEL_METHOD,
        "RUNTIME_JOB_METHODS": RUNTIME_JOB_METHODS,
    }))
}

fn normalize_schema(value: &mut Value) {
    remove_key_recursive(value, "$schema");
}

fn remove_key_recursive(value: &mut Value, key: &str) {
    match value {
        Value::Object(map) => {
            map.remove(key);
            for child in map.values_mut() {
                remove_key_recursive(child, key);
            }
        }
        Value::Array(items) => {
            for child in items {
                remove_key_recursive(child, key);
            }
        }
        _ => {}
    }
}

fn format_json(value: &Value) -> String {
    let mut output = serde_json::to_string_pretty(value).expect("format schema json");
    output.push('\n');
    output
}
