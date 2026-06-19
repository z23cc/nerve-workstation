//! MCP client — consume external MCP servers as tools through the
//! `RuntimeToolAdapter` seam (architecture north star P1: tools-as-plugins,
//! zero recompile, no new heavy dependency / no async runtime).

mod adapter;
mod client;
mod config;

use crate::tools::NerveRuntime;
use crate::workspace::ServeArgs;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;

/// Attach an MCP-client adapter to `runtime` when `--mcp-config` lists servers.
///
/// Connection happens eagerly; unreachable servers are logged and skipped, so a
/// bad server never breaks startup. A *name collision* — an ingested tool whose
/// namespaced name would shadow a core tool (or a duplicate server) — is instead
/// a hard error: we refuse to load rather than silently shadow. A no-op when no
/// servers are configured.
pub(crate) fn attach(runtime: NerveRuntime, args: &ServeArgs) -> Result<NerveRuntime> {
    let servers = config::load_from_args(args)?;
    if servers.is_empty() {
        return Ok(runtime);
    }
    // Names already claimed by the core runtime (and any prior adapters); an MCP
    // tool that would collide with one of these is a load error.
    let reserved = claimed_tool_names(&runtime);
    let adapter = adapter::McpClientToolAdapter::connect(servers, &reserved)?;
    if adapter.is_empty() {
        return Ok(runtime);
    }
    Ok(runtime.with_adapter(adapter))
}

/// Collect the tool names the runtime already exposes, so the MCP adapter can
/// reject a namespaced tool that would shadow one of them.
fn claimed_tool_names(runtime: &NerveRuntime) -> HashSet<String> {
    runtime
        .tool_specs()
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|spec| spec.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}
