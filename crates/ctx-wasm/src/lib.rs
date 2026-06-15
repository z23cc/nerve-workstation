//! wasm-bindgen surface for browser/edge hosts.
//!
//! Hosts feed file path/content pairs into an in-memory catalog and then pass
//! transport-neutral tool-call JSON through ctx-core dispatch.

use ctx_core::{HostFile, MemoryCatalogProvider, handle_tool_call_json};
use serde::Deserialize;
use std::cell::RefCell;
use wasm_bindgen::prelude::*;

thread_local! {
    static PROVIDER: RefCell<MemoryCatalogProvider> = RefCell::new(MemoryCatalogProvider::empty());
}

#[derive(Debug, Deserialize)]
struct FeedFile {
    path: String,
    content: String,
}

/// Replace the in-memory catalog with host-provided files.
///
/// `files_json` must be a JSON array of `{ "path": string, "content": string }`.
/// Paths are logical catalog paths, not filesystem paths; no filesystem scan or
/// ignore processing occurs.
#[wasm_bindgen]
pub fn feed_files(files_json: &str) -> Result<(), JsValue> {
    let files: Vec<FeedFile> = serde_json::from_str(files_json)
        .map_err(|err| JsValue::from_str(&format!("invalid feed_files json: {err}")))?;
    let host_files = files
        .into_iter()
        .map(|file| HostFile::new(file.path, file.content.into_bytes()))
        .collect();
    let provider = MemoryCatalogProvider::new(host_files)
        .map_err(|err| JsValue::from_str(&err.to_string()))?;
    PROVIDER.with(|slot| {
        *slot.borrow_mut() = provider;
    });
    Ok(())
}

/// Handle one ctx-core tool-call params JSON object and return JSON.
///
/// The input is the same transport-neutral shape accepted by
/// `ctx_core::handle_tool_call_json`, for example:
/// `{ "name": "file_search", "arguments": { "pattern": "needle" } }`.
#[wasm_bindgen]
pub fn handle_request(request_json: &str) -> String {
    PROVIDER.with(
        |slot| match handle_tool_call_json(&*slot.borrow(), request_json) {
            Ok(response) => response,
            Err(err) => {
                ctx_core::dispatch_error_json(ctx_core::dispatch_error_kind(&err), &err.to_string())
            }
        },
    )
}
