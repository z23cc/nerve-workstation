use crate::{CtxError, edit};
use serde_json::json;

/// Errors produced while decoding or dispatching a tool call.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("tools/call requires string name")]
    MissingToolName,
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    #[error(transparent)]
    Core(#[from] crate::CtxError),
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
        DispatchError::Core(CtxError::Cancelled) => "cancelled",
        DispatchError::Core(
            CtxError::AmbiguousWorkspace
            | CtxError::UnknownWorkspace(_)
            | CtxError::ManageWorkspacesUnsupported
            | CtxError::MissingWorkspaceName,
        ) => "workspace",
        DispatchError::Core(_) => "core",
        DispatchError::Json(_) => "json",
        DispatchError::Edit(_) => "edit",
    }
}

#[must_use]
pub fn dispatch_error_json(kind: &str, message: &str) -> String {
    json!({ "error": { "kind": kind, "message": message } }).to_string()
}
