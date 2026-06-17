use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, de::Error as _};
use serde_json::{Map, Value, json};
use std::io::Write;

#[derive(Debug)]
pub(crate) struct RpcMessage {
    pub(crate) id: Option<Value>,
    pub(crate) method: String,
    pub(crate) params: Value,
}

impl<'de> Deserialize<'de> for RpcMessage {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let mut object = value
            .as_object()
            .cloned()
            .ok_or_else(|| D::Error::custom("JSON-RPC message must be an object"))?;
        let method = take_required_string(&mut object, "method")?;
        let id = object.remove("id");
        let params = object.remove("params").unwrap_or_else(|| json!({}));
        Ok(Self { id, method, params })
    }
}

fn take_required_string<E>(
    object: &mut Map<String, Value>,
    key: &str,
) -> std::result::Result<String, E>
where
    E: serde::de::Error,
{
    object
        .remove(key)
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or_else(|| E::custom(format!("missing or invalid {key}")))
}

pub(crate) fn write_response(mut out: impl Write, value: Value) -> Result<()> {
    serde_json::to_writer(&mut out, &value).context("failed to encode response")?;
    writeln!(out).context("failed to write response")?;
    out.flush().context("failed to flush response")
}

pub(crate) fn jsonrpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub(crate) fn jsonrpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}
