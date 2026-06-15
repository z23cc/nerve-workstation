//! wasm-bindgen surface for browser/edge hosts.
//!
//! Hosts feed named workspace file path/content pairs into in-memory catalogs and
//! then pass transport-neutral tool-call JSON through ctx-core dispatch.

use ctx_core::{
    HostFile, MemoryCatalogProvider, WorkspaceRegistry, handle_tool_call_json_with_resolver,
};
use serde::Deserialize;
use std::sync::Arc;
use wasm_bindgen::prelude::*;

const DEFAULT_WORKSPACE: &str = "default";

thread_local! {
    static REGISTRY: WorkspaceRegistry<MemoryCatalogProvider> = WorkspaceRegistry::new();
}

#[derive(Debug, Deserialize)]
struct FeedFile {
    path: String,
    content: String,
}

/// Replace the default in-memory workspace with host-provided files.
///
/// `files_json` must be a JSON array of `{ "path": string, "content": string }`.
/// Paths are logical catalog paths, not filesystem paths; no filesystem scan or
/// ignore processing occurs. This preserves the original single-workspace API by
/// registering the files under the `default` workspace.
#[wasm_bindgen]
pub fn feed_files(files_json: &str) -> Result<(), JsValue> {
    feed_workspace(DEFAULT_WORKSPACE, files_json)
}

/// Replace one named in-memory workspace with host-provided files.
///
/// `workspace_name` is the value later supplied as tool-call `arguments.workspace`.
/// `files_json` must be a JSON array of `{ "path": string, "content": string }`.
#[wasm_bindgen]
pub fn feed_workspace(workspace_name: &str, files_json: &str) -> Result<(), JsValue> {
    if workspace_name.is_empty() {
        return Err(JsValue::from_str("workspace_name must not be empty"));
    }
    let files: Vec<FeedFile> = serde_json::from_str(files_json)
        .map_err(|err| JsValue::from_str(&format!("invalid feed_workspace json: {err}")))?;
    let host_files = files
        .into_iter()
        .map(|file| HostFile::new(file.path, file.content.into_bytes()))
        .collect();
    let provider = MemoryCatalogProvider::new(host_files)
        .map_err(|err| JsValue::from_str(&err.to_string()))?;
    REGISTRY.with(|registry| {
        registry.insert(workspace_name, Arc::new(provider));
    });
    Ok(())
}

/// Handle one ctx-core tool-call params JSON object and return JSON.
///
/// The input is the same transport-neutral shape accepted by
/// `ctx_core::handle_tool_call_json_with_resolver`, for example:
/// `{ "name": "file_search", "arguments": { "workspace": "default", "pattern": "needle" } }`.
#[wasm_bindgen]
pub fn handle_request(request_json: &str) -> String {
    REGISTRY.with(
        |registry| match handle_tool_call_json_with_resolver(registry, request_json) {
            Ok(response) => response,
            Err(err) => {
                ctx_core::dispatch_error_json(ctx_core::dispatch_error_kind(&err), &err.to_string())
            }
        },
    )
}
