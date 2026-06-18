mod http;
mod router;
mod setup;
mod stdio;

use crate::workspace;
use anyhow::{Result, bail};
use clap::Args;
use std::net::SocketAddr;

#[derive(Debug, Args)]
pub(crate) struct RuntimeDaemonArgs {
    /// Run the daemon over line-delimited JSON-RPC on stdin/stdout.
    #[arg(long)]
    stdio: bool,
    /// Serve the same Protocol v3 over HTTP for browser/GUI clients:
    /// `POST /rpc` (JSON-RPC) and `GET /events` (SSE). Binds the given loopback
    /// address (default `127.0.0.1:4173`). Mutually exclusive with `--stdio`.
    #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = "127.0.0.1:4173")]
    http: Option<SocketAddr>,
    #[command(flatten)]
    serve: workspace::ServeArgs,
}

pub(crate) fn run(args: RuntimeDaemonArgs) -> Result<()> {
    // One transport per run: the stdio and HTTP transports drive the same
    // router but own their process I/O exclusively.
    match (args.stdio, args.http) {
        (true, Some(_)) => bail!("choose a single daemon transport: --stdio or --http <addr>"),
        (false, None) => bail!("daemon requires a transport: --stdio or --http <addr>"),
        (true, None) => stdio::run_stdio(args.serve),
        (false, Some(addr)) => http::run_http(args.serve, addr),
    }
}

#[cfg(test)]
mod tests;
