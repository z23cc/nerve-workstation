//! Transport-neutral MCP tool dispatch for the context engine.

use crate::{
    CancelToken, CatalogProvider, CtxError, ReadFileRequest, RepoMapRequest, SearchMode,
    SearchRequest, SingletonWorkspaceResolver, WorkspaceResolver, build_context_cancellable,
    get_code_structure, get_file_tree, get_repo_map_cancellable, manage_selection, read_file,
    search_snapshot_cancellable, workspace_context,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

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
}

/// Return the MCP tool specifications supported by the engine.
#[must_use]
pub fn tool_specs() -> Value {
    let tools = vec![
        json!({
            "name": "build_context",
            "description": "Build a deterministic query-focused context within a token budget.",
            "inputSchema": {
                "type": "object",
                "required": ["query", "token_budget"],
                "properties": {
                    "workspace": workspace_schema(),
                    "query": {
                        "type": "string",
                        "description": "Search query used for file ranking and repo-map personalization."
                    },
                    "token_budget": {
                        "type": "integer",
                        "description": "Maximum assembled context tokens."
                    },
                    "max_files": {
                        "type": "integer",
                        "default": 20,
                        "description": "Maximum number of files to include."
                    }
                }
            }
        }),
        json!({
            "name": "workspace_context",
            "description": "Assemble the current persistent selection into context text with token breakdowns.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspace": workspace_schema(),
                    "include": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["file-map", "contents", "tokens"] },
                        "description": "Optional text sections to include. Empty means file-map and contents."
                    },
                    "instructions": {
                        "type": "string",
                        "description": "Optional notes/instructions to include in the context snapshot."
                    }
                }
            }
        }),
        json!({
            "name": "manage_selection",
            "description": "Persist and summarize the selected file set with token estimates.",
            "inputSchema": {
                "type": "object",
                "required": ["op"],
                "properties": {
                    "workspace": workspace_schema(),
                    "op": { "type": "string", "enum": ["get", "add", "remove", "set", "clear"] },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "File or directory paths relative to an allowed root, or in-root absolute paths."
                    },
                    "mode": { "type": "string", "enum": ["full", "slices", "codemap_only"], "default": "full" },
                    "slices": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["path"],
                            "properties": {
                                "path": { "type": "string" },
                                "ranges": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "required": ["start_line", "end_line"],
                                        "properties": {
                                            "start_line": { "type": "integer" },
                                            "end_line": { "type": "integer" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }),
        json!({
            "name": "file_search",
            "description": "Search allowed roots by path and/or file content.",
            "inputSchema": {
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "workspace": workspace_schema(),
                    "pattern": { "type": "string" },
                    "mode": { "type": "string", "enum": ["path", "content", "both"], "default": "both" },
                    "regex": { "type": "boolean", "default": false },
                    "max_results": { "type": "integer", "default": 50 },
                    "context_lines": { "type": "integer", "default": 2 },
                    "max_content_files": { "type": "integer", "default": 2048 },
                    "max_content_bytes": { "type": "integer", "default": 67108864 },
                    "whole_word": { "type": "boolean", "default": false }
                }
            }
        }),
        json!({
            "name": "read_file",
            "description": "Read a file from allowed roots with optional line range.",
            "inputSchema": {
                "type": "object",
                "required": ["path"],
                "properties": {
                    "workspace": workspace_schema(),
                    "path": { "type": "string" },
                    "start_line": { "type": "integer" },
                    "end_line": { "type": "integer" }
                }
            }
        }),
        json!({
            "name": "get_file_tree",
            "description": "Return a compact tree for allowed roots.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspace": workspace_schema(),
                    "max_depth": { "type": "integer", "default": 3 }
                }
            }
        }),
        json!({
            "name": "get_code_structure",
            "description": "Return lightweight top-level code symbols for supported source files.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspace": workspace_schema(),
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional file or directory paths relative to an allowed root. Empty means whole catalog."
                    }
                }
            }
        }),
        json!({
            "name": "get_repo_map",
            "description": "Rank relevant repository files with deterministic personalized PageRank over codemap symbol references.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspace": workspace_schema(),
                    "query": {
                        "type": "string",
                        "description": "Optional literal query. Matching indexed files become personalized PageRank seeds."
                    },
                    "seed_paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional explicit file or directory seed paths, relative to an allowed root or absolute."
                    },
                    "max_files": { "type": "integer", "default": 20 }
                }
            }
        }),
    ];
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut tools = tools;
        tools.push(json!({
            "name": "manage_workspaces",
            "description": "List, add, remove, or inspect registered filesystem workspaces.",
            "inputSchema": {
                "type": "object",
                "required": ["op"],
                "properties": {
                    "op": { "type": "string", "enum": ["list", "add", "remove", "get"] },
                    "name": {
                        "type": "string",
                        "description": "Workspace name for add, remove, or get."
                    },
                    "roots": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Allowed roots for add. Empty roots are fail-closed."
                    }
                }
            }
        }));
        Value::Array(tools)
    }
    #[cfg(target_arch = "wasm32")]
    {
        Value::Array(tools)
    }
}

/// Dispatch one MCP `tools/call` params object and return the MCP tool response.
pub fn handle_tool_call<P>(provider: &P, params: &Value) -> Result<Value, DispatchError>
where
    P: CatalogProvider + Sync,
{
    handle_tool_call_cancellable(provider, params, &CancelToken::never())
}

/// Dispatch one MCP `tools/call` params object with cooperative cancellation.
pub fn handle_tool_call_cancellable<P>(
    provider: &P,
    params: &Value,
    cancel: &CancelToken,
) -> Result<Value, DispatchError>
where
    P: CatalogProvider + Sync,
{
    let resolver = SingletonWorkspaceResolver::new(provider);
    handle_tool_call_with_resolver_cancellable(&resolver, params, cancel)
}

/// Dispatch one MCP `tools/call` params object through a workspace resolver.
pub fn handle_tool_call_with_resolver<R>(
    resolver: &R,
    params: &Value,
) -> Result<Value, DispatchError>
where
    R: WorkspaceResolver,
{
    handle_tool_call_with_resolver_cancellable(resolver, params, &CancelToken::never())
}

/// Dispatch one MCP `tools/call` params object through a workspace resolver with cancellation.
pub fn handle_tool_call_with_resolver_cancellable<R>(
    resolver: &R,
    params: &Value,
    cancel: &CancelToken,
) -> Result<Value, DispatchError>
where
    R: WorkspaceResolver,
{
    cancel.check_cancelled()?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or(DispatchError::MissingToolName)?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    #[cfg(not(target_arch = "wasm32"))]
    if name == "manage_workspaces" {
        let args: crate::ManageWorkspacesRequest = serde_json::from_value(arguments)?;
        let structured = serde_json::to_value(resolver.manage_workspaces(args)?)?;
        return tool_response(structured);
    }
    let workspace = workspace_arg(&arguments)?;
    let provider = resolver.resolve_workspace(workspace)?;
    let provider = &*provider;
    let structured = match name {
        "manage_selection" => {
            let args: crate::ManageSelectionRequest = serde_json::from_value(arguments)?;
            let snapshot = provider.snapshot_arc_cancellable(cancel)?;
            cancel.check_cancelled()?;
            let response = manage_selection(provider, &snapshot, &args)?;
            cancel.check_cancelled()?;
            serde_json::to_value(response)?
        }
        "workspace_context" => {
            let args: crate::WorkspaceContextRequest = serde_json::from_value(arguments)?;
            let snapshot = provider.snapshot_arc_cancellable(cancel)?;
            cancel.check_cancelled()?;
            let response = workspace_context(provider, &snapshot, &args)?;
            cancel.check_cancelled()?;
            serde_json::to_value(response)?
        }
        "build_context" => {
            let args: crate::BuildContextRequest = serde_json::from_value(arguments)?;
            let snapshot = provider.snapshot_arc_cancellable(cancel)?;
            cancel.check_cancelled()?;
            let response = build_context_cancellable(provider, &snapshot, &args, cancel)?;
            cancel.check_cancelled()?;
            serde_json::to_value(response)?
        }
        "file_search" => {
            let args: FileSearchArgs = serde_json::from_value(arguments)?;
            let snapshot = provider.snapshot_arc_cancellable(cancel)?;
            let response =
                search_snapshot_cancellable(provider, &snapshot, &args.into_request(), cancel)?;
            serde_json::to_value(response)?
        }
        "read_file" => {
            cancel.check_cancelled()?;
            let args: ReadFileArgs = serde_json::from_value(arguments)?;
            let response = read_file(provider, &args.into_request())?;
            cancel.check_cancelled()?;
            serde_json::to_value(response)?
        }
        "get_file_tree" => {
            let args: FileTreeArgs = serde_json::from_value(arguments)?;
            let snapshot = provider.snapshot_arc_cancellable(cancel)?;
            cancel.check_cancelled()?;
            serde_json::to_value(get_file_tree(&snapshot, args.max_depth.unwrap_or(3)))?
        }
        "get_code_structure" => {
            let args: CodeStructureArgs = serde_json::from_value(arguments)?;
            let snapshot = provider.snapshot_arc_cancellable(cancel)?;
            cancel.check_cancelled()?;
            let response =
                get_code_structure(provider, &snapshot, &args.paths.unwrap_or_default())?;
            cancel.check_cancelled()?;
            serde_json::to_value(response)?
        }
        "get_repo_map" => {
            let args: RepoMapArgs = serde_json::from_value(arguments)?;
            let snapshot = provider.snapshot_arc_cancellable(cancel)?;
            let response =
                get_repo_map_cancellable(provider, &snapshot, &args.into_request(), cancel)?;
            serde_json::to_value(response)?
        }
        other => return Err(DispatchError::UnknownTool(other.to_string())),
    };

    tool_response(structured)
}

fn tool_response(structured: Value) -> Result<Value, DispatchError> {
    Ok(json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&structured)? }],
        "structuredContent": structured,
    }))
}

/// Decode one JSON tool-call params object and encode the tool response as JSON.
pub fn handle_tool_call_json<P>(provider: &P, request_json: &str) -> Result<String, DispatchError>
where
    P: CatalogProvider + Sync,
{
    handle_tool_call_json_cancellable(provider, request_json, &CancelToken::never())
}

/// Decode one JSON tool-call params object and encode the tool response as JSON,
/// returning a JSON error object for cooperative cancellation.
pub fn handle_tool_call_json_cancellable<P>(
    provider: &P,
    request_json: &str,
    cancel: &CancelToken,
) -> Result<String, DispatchError>
where
    P: CatalogProvider + Sync,
{
    let resolver = SingletonWorkspaceResolver::new(provider);
    handle_tool_call_json_with_resolver_cancellable(&resolver, request_json, cancel)
}

/// Decode one JSON tool-call params object and encode the tool response through a resolver.
pub fn handle_tool_call_json_with_resolver<R>(
    resolver: &R,
    request_json: &str,
) -> Result<String, DispatchError>
where
    R: WorkspaceResolver,
{
    handle_tool_call_json_with_resolver_cancellable(resolver, request_json, &CancelToken::never())
}

/// Decode one JSON tool-call params object and encode the resolver-routed tool response,
/// returning a JSON error object for cooperative cancellation.
pub fn handle_tool_call_json_with_resolver_cancellable<R>(
    resolver: &R,
    request_json: &str,
    cancel: &CancelToken,
) -> Result<String, DispatchError>
where
    R: WorkspaceResolver,
{
    let params: Value = serde_json::from_str(request_json)?;
    match handle_tool_call_with_resolver_cancellable(resolver, &params, cancel) {
        Ok(response) => Ok(serde_json::to_string(&response)?),
        Err(err) if matches!(err, DispatchError::Core(CtxError::Cancelled)) => Ok(
            dispatch_error_json(dispatch_error_kind(&err), &err.to_string()),
        ),
        Err(err) => Err(err),
    }
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
    }
}

#[must_use]
pub fn dispatch_error_json(kind: &str, message: &str) -> String {
    json!({ "error": { "kind": kind, "message": message } }).to_string()
}

#[must_use]
fn workspace_schema() -> Value {
    json!({
        "type": "string",
        "description": "Optional workspace id to route this tool call. Required when multiple workspaces are registered."
    })
}

fn workspace_arg(arguments: &Value) -> Result<Option<&str>, DispatchError> {
    arguments
        .get("workspace")
        .map(|value| {
            value.as_str().ok_or_else(|| {
                serde_json::Error::io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "workspace must be a string",
                ))
            })
        })
        .transpose()
        .map_err(DispatchError::Json)
}

#[derive(Debug, Deserialize)]
struct FileSearchArgs {
    pattern: String,
    #[serde(default = "default_mode")]
    mode: String,
    #[serde(default)]
    regex: bool,
    #[serde(default = "default_max_results")]
    max_results: usize,
    #[serde(default = "default_context_lines")]
    context_lines: usize,
    #[serde(default = "default_max_content_files")]
    max_content_files: usize,
    #[serde(default = "default_max_content_bytes")]
    max_content_bytes: u64,
    #[serde(default)]
    whole_word: bool,
}

impl FileSearchArgs {
    fn into_request(self) -> SearchRequest {
        SearchRequest {
            pattern: self.pattern,
            mode: match self.mode.as_str() {
                "path" => SearchMode::Path,
                "content" => SearchMode::Content,
                _ => SearchMode::Both,
            },
            regex: self.regex,
            max_results: self.max_results,
            context_lines: self.context_lines,
            max_content_files: self.max_content_files,
            max_content_bytes: self.max_content_bytes,
            whole_word: self.whole_word,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReadFileArgs {
    path: PathBuf,
    start_line: Option<usize>,
    end_line: Option<usize>,
    limit: Option<usize>,
}

impl ReadFileArgs {
    fn into_request(self) -> ReadFileRequest {
        ReadFileRequest {
            path: self.path,
            start_line: self.start_line,
            end_line: self.end_line,
            limit: self.limit,
        }
    }
}

#[derive(Debug, Deserialize)]
struct FileTreeArgs {
    max_depth: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct CodeStructureArgs {
    paths: Option<Vec<PathBuf>>,
}

#[derive(Debug, Deserialize)]
struct RepoMapArgs {
    query: Option<String>,
    #[serde(default)]
    seed_paths: Vec<PathBuf>,
    #[serde(default = "default_repo_map_max_files")]
    max_files: usize,
}

impl RepoMapArgs {
    fn into_request(self) -> RepoMapRequest {
        RepoMapRequest {
            query: self.query,
            seed_paths: self.seed_paths,
            max_files: self.max_files,
        }
    }
}

fn default_repo_map_max_files() -> usize {
    20
}

fn default_mode() -> String {
    "both".to_string()
}

fn default_max_results() -> usize {
    50
}

fn default_context_lines() -> usize {
    2
}

fn default_max_content_files() -> usize {
    2_048
}

fn default_max_content_bytes() -> u64 {
    64 * 1024 * 1024
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FsCatalogProvider, RootPolicy, ScanOptions, WorkspaceRegistry};
    use std::{fs, sync::Arc};

    fn provider_for(path: &std::path::Path) -> FsCatalogProvider {
        FsCatalogProvider::new(
            RootPolicy::new(vec![path.to_path_buf()]).expect("policy"),
            ScanOptions::default(),
        )
    }

    #[test]
    fn resolver_routes_explicit_workspace() {
        let left = tempfile::tempdir().expect("left tempdir");
        let right = tempfile::tempdir().expect("right tempdir");
        fs::write(left.path().join("left.txt"), "alpha\n").expect("left write");
        fs::write(right.path().join("right.txt"), "beta\n").expect("right write");

        let registry: WorkspaceRegistry<FsCatalogProvider> = WorkspaceRegistry::new();
        registry.insert("left", Arc::new(provider_for(left.path())));
        registry.insert("right", Arc::new(provider_for(right.path())));

        let response = handle_tool_call_with_resolver(
            &registry,
            &json!({
                "name": "file_search",
                "arguments": {
                    "workspace": "right",
                    "pattern": "beta",
                    "mode": "content"
                }
            }),
        )
        .expect("workspace-routed search");

        assert_eq!(
            response["structuredContent"]["content_matches"][0]["path"],
            Value::String("right.txt".to_string())
        );
    }

    #[test]
    fn singleton_provider_ignores_default_and_explicit_workspace() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("text.txt"), "needle\n").expect("write");
        let provider = provider_for(dir.path());

        let default_response = handle_tool_call(
            &provider,
            &json!({
                "name": "file_search",
                "arguments": { "pattern": "needle", "mode": "content" }
            }),
        )
        .expect("default singleton search");
        let explicit_response = handle_tool_call(
            &provider,
            &json!({
                "name": "file_search",
                "arguments": { "workspace": "anything", "pattern": "needle", "mode": "content" }
            }),
        )
        .expect("explicit singleton search");

        assert_eq!(
            default_response["structuredContent"]["content_matches"][0]["path"],
            explicit_response["structuredContent"]["content_matches"][0]["path"]
        );
    }

    #[test]
    fn registry_without_workspace_is_ambiguous_when_multiple_exist() {
        let left = tempfile::tempdir().expect("left tempdir");
        let right = tempfile::tempdir().expect("right tempdir");
        fs::write(left.path().join("left.txt"), "alpha\n").expect("left write");
        fs::write(right.path().join("right.txt"), "beta\n").expect("right write");

        let registry: WorkspaceRegistry<FsCatalogProvider> = WorkspaceRegistry::new();
        registry.insert("left", Arc::new(provider_for(left.path())));
        registry.insert("right", Arc::new(provider_for(right.path())));

        let err = handle_tool_call_with_resolver(
            &registry,
            &json!({
                "name": "file_search",
                "arguments": { "pattern": "alpha", "mode": "content" }
            }),
        )
        .expect_err("ambiguous workspace should fail");

        assert!(matches!(
            err,
            DispatchError::Core(CtxError::AmbiguousWorkspace)
        ));
        assert_eq!(err.to_string(), "ambiguous: specify workspace");
    }

    #[test]
    fn registry_singleton_default_routes_to_only_workspace() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("only.txt"), "needle\n").expect("write");

        let registry: WorkspaceRegistry<FsCatalogProvider> = WorkspaceRegistry::new();
        registry.insert("only", Arc::new(provider_for(dir.path())));

        let response = handle_tool_call_with_resolver(
            &registry,
            &json!({
                "name": "file_search",
                "arguments": { "pattern": "needle", "mode": "content" }
            }),
        )
        .expect("singleton registry default search");

        assert_eq!(
            response["structuredContent"]["content_matches"][0]["path"],
            Value::String("only.txt".to_string())
        );
    }

    #[test]
    fn manage_workspaces_add_remove_and_routes_new_workspace() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("added.txt"),
            "dynamic
",
        )
        .expect("write");
        let registry: WorkspaceRegistry<FsCatalogProvider> = WorkspaceRegistry::new();

        let specs = tool_specs();
        let tool_names: Vec<_> = specs
            .as_array()
            .expect("tool specs array")
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect();
        assert!(tool_names.contains(&"manage_workspaces"));

        let add = handle_tool_call_with_resolver(
            &registry,
            &json!({
                "name": "manage_workspaces",
                "arguments": {
                    "op": "add",
                    "name": "dynamic",
                    "roots": [dir.path()]
                }
            }),
        )
        .expect("add workspace");
        assert_eq!(
            add["structuredContent"]["workspaces"][0]["name"],
            Value::String("dynamic".to_string())
        );

        let search = handle_tool_call_with_resolver(
            &registry,
            &json!({
                "name": "file_search",
                "arguments": {
                    "workspace": "dynamic",
                    "pattern": "dynamic",
                    "mode": "content"
                }
            }),
        )
        .expect("search added workspace");
        assert_eq!(
            search["structuredContent"]["content_matches"][0]["path"],
            Value::String("added.txt".to_string())
        );

        handle_tool_call_with_resolver(
            &registry,
            &json!({
                "name": "manage_workspaces",
                "arguments": { "op": "remove", "name": "dynamic" }
            }),
        )
        .expect("remove workspace");
        let err = handle_tool_call_with_resolver(
            &registry,
            &json!({
                "name": "file_search",
                "arguments": { "workspace": "dynamic", "pattern": "dynamic" }
            }),
        )
        .expect_err("removed workspace should not route");
        assert!(matches!(
            err,
            DispatchError::Core(CtxError::UnknownWorkspace(_))
        ));
    }

    #[test]
    fn workspaces_keep_selection_and_search_isolated() {
        let left = tempfile::tempdir().expect("left tempdir");
        let right = tempfile::tempdir().expect("right tempdir");
        fs::write(left.path().join("left.txt"), "alpha\n").expect("left write");
        fs::write(right.path().join("right.txt"), "beta\n").expect("right write");

        let registry: WorkspaceRegistry<FsCatalogProvider> = WorkspaceRegistry::new();
        registry.insert("left", Arc::new(provider_for(left.path())));
        registry.insert("right", Arc::new(provider_for(right.path())));

        handle_tool_call_with_resolver(
            &registry,
            &json!({
                "name": "manage_selection",
                "arguments": {
                    "workspace": "left",
                    "op": "set",
                    "paths": ["left.txt"],
                    "mode": "full"
                }
            }),
        )
        .expect("set left selection");
        handle_tool_call_with_resolver(
            &registry,
            &json!({
                "name": "manage_selection",
                "arguments": {
                    "workspace": "right",
                    "op": "set",
                    "paths": ["right.txt"],
                    "mode": "full"
                }
            }),
        )
        .expect("set right selection");

        let left_selection = handle_tool_call_with_resolver(
            &registry,
            &json!({
                "name": "manage_selection",
                "arguments": { "workspace": "left", "op": "get" }
            }),
        )
        .expect("get left selection");
        let right_selection = handle_tool_call_with_resolver(
            &registry,
            &json!({
                "name": "manage_selection",
                "arguments": { "workspace": "right", "op": "get" }
            }),
        )
        .expect("get right selection");

        assert_eq!(
            left_selection["structuredContent"]["files"][0]["path"],
            Value::String("left.txt".to_string())
        );
        assert_eq!(
            right_selection["structuredContent"]["files"][0]["path"],
            Value::String("right.txt".to_string())
        );

        let wrong_workspace_search = handle_tool_call_with_resolver(
            &registry,
            &json!({
                "name": "file_search",
                "arguments": { "workspace": "left", "pattern": "beta", "mode": "content" }
            }),
        )
        .expect("search left for right content");
        let right_search = handle_tool_call_with_resolver(
            &registry,
            &json!({
                "name": "file_search",
                "arguments": { "workspace": "right", "pattern": "beta", "mode": "content" }
            }),
        )
        .expect("search right content");

        assert_eq!(
            wrong_workspace_search["structuredContent"]["content_matches"]
                .as_array()
                .expect("left matches")
                .len(),
            0
        );
        assert_eq!(
            right_search["structuredContent"]["content_matches"][0]["path"],
            Value::String("right.txt".to_string())
        );
    }

    #[test]
    fn cancellable_json_dispatch_returns_cancelled_error_object() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("text.txt"), "needle\n").expect("write");
        let provider = FsCatalogProvider::new(
            RootPolicy::new(vec![dir.path().to_path_buf()]).expect("policy"),
            ScanOptions::default(),
        );
        let token = CancelToken::new();
        token.cancel();

        let json = handle_tool_call_json_cancellable(
            &provider,
            r#"{"name":"file_search","arguments":{"pattern":"needle","mode":"content"}}"#,
            &token,
        )
        .expect("cancelled dispatch is encoded as JSON");
        let value: Value = serde_json::from_str(&json).expect("json");
        assert_eq!(value["error"]["kind"], "cancelled");
    }

    #[test]
    fn manage_selection_is_listed_dispatches_and_persists() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("text.txt"), "one\ntwo\n").expect("write");
        let provider = FsCatalogProvider::new(
            RootPolicy::new(vec![dir.path().to_path_buf()]).expect("policy"),
            ScanOptions::default(),
        );

        let specs = tool_specs();
        let tool_names: Vec<_> = specs
            .as_array()
            .expect("tool specs array")
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect();
        assert!(tool_names.contains(&"manage_selection"));

        let set_response = handle_tool_call(
            &provider,
            &json!({
                "name": "manage_selection",
                "arguments": {
                    "op": "set",
                    "paths": ["text.txt"],
                    "mode": "full"
                }
            }),
        )
        .expect("selection dispatch");
        assert_eq!(
            set_response["structuredContent"]["files"][0]["path"],
            Value::String("text.txt".to_string())
        );
        assert!(
            set_response["structuredContent"]["files"][0]["token_estimate"]
                .as_u64()
                .expect("token count")
                > 0
        );

        let get_response = handle_tool_call(
            &provider,
            &json!({
                "name": "manage_selection",
                "arguments": { "op": "get" }
            }),
        )
        .expect("selection get");
        assert_eq!(
            get_response["structuredContent"]["files"][0]["path"],
            Value::String("text.txt".to_string())
        );
    }

    #[test]
    fn workspace_context_is_listed_and_dispatches() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("text.txt"), "one\ntwo\n").expect("write");
        let provider = FsCatalogProvider::new(
            RootPolicy::new(vec![dir.path().to_path_buf()]).expect("policy"),
            ScanOptions::default(),
        );

        let specs = tool_specs();
        let tool_names: Vec<_> = specs
            .as_array()
            .expect("tool specs array")
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect();
        assert!(tool_names.contains(&"workspace_context"));

        handle_tool_call(
            &provider,
            &json!({
                "name": "manage_selection",
                "arguments": {
                    "op": "set",
                    "paths": ["text.txt"],
                    "mode": "full"
                }
            }),
        )
        .expect("selection dispatch");

        let response = handle_tool_call(
            &provider,
            &json!({
                "name": "workspace_context",
                "arguments": {
                    "include": ["file-map", "contents", "tokens"],
                    "instructions": "Use this context."
                }
            }),
        )
        .expect("workspace context dispatch");
        let structured = &response["structuredContent"];
        assert!(
            structured["context"]
                .as_str()
                .expect("context")
                .contains("<file_map>")
        );
        assert!(
            structured["context"]
                .as_str()
                .expect("context")
                .contains("<tokens>")
        );
        assert_eq!(
            structured["tokens"]["files"][0]["path"],
            Value::String("text.txt".to_string())
        );
    }

    #[test]
    fn build_context_is_listed_dispatches_and_preserves_selection() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("text.txt"), "needle\n").expect("write");
        let provider = FsCatalogProvider::new(
            RootPolicy::new(vec![dir.path().to_path_buf()]).expect("policy"),
            ScanOptions::default(),
        );
        let before = provider.selection();

        let specs = tool_specs();
        let tool_names: Vec<_> = specs
            .as_array()
            .expect("tool specs array")
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect();
        assert!(tool_names.contains(&"build_context"));

        let response = handle_tool_call(
            &provider,
            &json!({
                "name": "build_context",
                "arguments": {
                    "query": "needle",
                    "token_budget": 100,
                    "max_files": 1
                }
            }),
        )
        .expect("build context dispatch");
        let structured = &response["structuredContent"];
        assert_eq!(
            structured["manifest"]["included"][0]["path"],
            Value::String("text.txt".to_string())
        );
        assert_eq!(provider.selection(), before);
    }
}
