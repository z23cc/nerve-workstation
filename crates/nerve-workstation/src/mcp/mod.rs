//! MCP client — consume external MCP servers as tools through the
//! `RuntimeToolAdapter` seam (architecture north star P1: tools-as-plugins,
//! zero recompile, no new heavy dependency / no async runtime).

mod adapter;
mod client;
mod config;

use crate::tools::NerveRuntime;
use crate::workspace::ServeArgs;
use anyhow::Result;

/// Attach an MCP-client adapter to `runtime` when `--mcp-config` lists servers.
///
/// Connection happens eagerly; unreachable servers are logged and skipped, so a
/// bad server never breaks startup. A no-op when no servers are configured.
pub(crate) fn attach(runtime: NerveRuntime, args: &ServeArgs) -> Result<NerveRuntime> {
    let servers = config::load_from_args(args)?;
    if servers.is_empty() {
        return Ok(runtime);
    }
    let adapter = adapter::McpClientToolAdapter::connect(servers);
    if adapter.is_empty() {
        return Ok(runtime);
    }
    Ok(runtime.with_adapter(adapter))
}
