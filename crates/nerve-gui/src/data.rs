//! GUI data fetchers: read real workspace / git / file-tree facts via the
//! `tool.call` command (the daemon's snapshot-backed tools), so the chrome
//! shows live data instead of placeholders. Each helper runs one `tool.call`
//! job to completion and returns the tool's `structuredContent`. Kept out of
//! `app.rs` to stay under the file-size gate.

use crate::rpc::start_job_await;
use serde_json::{Value, json};

/// Run one `tool.call` job to completion; return its `structuredContent` object.
pub async fn tool_job(token: &str, name: &str, arguments: Value) -> Result<Value, String> {
    let result = start_job_await(
        token,
        json!({ "kind": "tool.call", "name": name, "arguments": arguments }),
    )
    .await?;
    result
        .get("structuredContent")
        .cloned()
        .ok_or_else(|| format!("{name}: no structuredContent"))
}

/// The default workspace name + first root, via `manage_workspaces {op:get}`.
pub async fn fetch_workspace(token: &str) -> Option<(String, String)> {
    let sc = tool_job(token, "manage_workspaces", json!({"op":"get","name":"default"}))
        .await
        .ok()?;
    let ws = sc.get("workspaces")?.as_array()?.first()?;
    let name = ws.get("name")?.as_str()?.to_string();
    let root = ws
        .get("roots")
        .and_then(|r| r.as_array())
        .and_then(|r| r.first())
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Some((name, root))
}

/// The current git branch, parsed from the `git {op:status}` porcelain header
/// (`## <branch>...origin/<branch> [ahead N]`).
pub async fn fetch_branch(token: &str) -> Option<String> {
    let sc = tool_job(token, "git", json!({"op":"status"})).await.ok()?;
    let output = sc.get("output")?.as_str()?;
    let header = output.lines().next()?.strip_prefix("## ")?;
    let branch = header
        .split_once("...")
        .map(|(b, _)| b)
        .unwrap_or(header)
        .split_whitespace()
        .next()
        .unwrap_or(header);
    Some(branch.to_string())
}

/// The repo file tree (ASCII), via `get_file_tree`.
pub async fn fetch_file_tree(token: &str) -> Option<String> {
    let sc = tool_job(token, "get_file_tree", json!({"mode":"auto","max_depth":3}))
        .await
        .ok()?;
    sc.get("tree")?.as_str().map(str::to_string)
}

/// The working-tree diff (unified), via `git {op:diff}`. Returns the diff text,
/// or a short note when the tree is clean.
pub async fn fetch_diff(token: &str) -> Option<String> {
    let sc = tool_job(token, "git", json!({"op":"diff"})).await.ok()?;
    let out = sc.get("output")?.as_str()?.trim();
    Some(if out.is_empty() {
        "No changes in the working tree.".to_string()
    } else {
        out.to_string()
    })
}
