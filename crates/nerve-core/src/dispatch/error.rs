use crate::{NerveError, edit};
use serde_json::json;

/// Errors produced while decoding or dispatching a tool call.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("tools/call requires string name")]
    MissingToolName,
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    #[error(transparent)]
    Core(#[from] crate::NerveError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Edit(#[from] edit::EditError),
}
#[must_use]
pub fn dispatch_error_kind(err: &DispatchError) -> &'static str {
    match err {
        DispatchError::MissingToolName => "missing_tool_name",
        DispatchError::UnknownTool(_) => "unknown_tool",
        DispatchError::Core(NerveError::Cancelled) => "cancelled",
        DispatchError::Core(
            NerveError::AmbiguousWorkspace
            | NerveError::UnknownWorkspace(_)
            | NerveError::ManageWorkspacesUnsupported
            | NerveError::MissingWorkspaceName,
        ) => "workspace",
        DispatchError::Core(_) => "core",
        DispatchError::Json(_) => "json",
        DispatchError::Edit(edit::EditError::StaleHash { .. }) => "stale_hash",
        DispatchError::Edit(_) => "edit",
    }
}

#[must_use]
pub fn dispatch_error_json(kind: &str, message: &str) -> String {
    json!({ "error": { "kind": kind, "message": message } }).to_string()
}

#[must_use]
pub fn dispatch_error_value(err: &DispatchError) -> serde_json::Value {
    match err {
        DispatchError::Edit(edit::EditError::StaleHash {
            path,
            expected,
            actual,
            reread_hint,
        }) => json!({
            "error": {
                "kind": dispatch_error_kind(err),
                "message": err.to_string(),
                "path": path,
                "expected_hash": expected,
                "actual_hash": actual,
                "reread_hint": reread_hint,
            }
        }),
        _ => json!({
            "error": {
                "kind": dispatch_error_kind(err),
                "message": err.to_string(),
            }
        }),
    }
}

#[must_use]
pub fn dispatch_error_json_for(err: &DispatchError) -> String {
    dispatch_error_value(err).to_string()
}
