use super::*;
use crate::{FsCatalogProvider, RootPolicy, ScanOptions, WorkspaceRegistry};
use std::{fs, sync::Arc};

fn provider_for(path: &std::path::Path) -> FsCatalogProvider {
    FsCatalogProvider::new(
        RootPolicy::new(vec![path.to_path_buf()]).expect("policy"),
        ScanOptions::default(),
    )
}

fn set_slice_selection(provider: &FsCatalogProvider, path: &str, start: usize, end: usize) {
    handle_tool_call(
        provider,
        &json!({ "name": "manage_selection", "arguments": {
            "op": "set", "mode": "slices",
            "slices": [{ "path": path, "ranges": [{ "start_line": start, "end_line": end }] }]
        } }),
    )
    .expect("set slice selection");
}

fn set_full_selection(provider: &FsCatalogProvider, path: &str) {
    handle_tool_call(
        provider,
        &json!({ "name": "manage_selection", "arguments": {
            "op": "set", "mode": "full", "paths": [path]
        } }),
    )
    .expect("set full selection");
}

fn selection_response(provider: &FsCatalogProvider) -> Value {
    handle_tool_call(
        provider,
        &json!({ "name": "manage_selection", "arguments": { "op": "get" } }),
    )
    .expect("selection get")
}

// Regression: every numeric tool parameter must tolerate integer-valued
// strings (clients that stringify numbers), per the documented contract.
// build_context.token_budget/max_files and ast_search.max_results were the
// two holdouts.

mod args_text;
mod editing;
mod editing_selection;
mod tool_contracts;
mod workspace;
