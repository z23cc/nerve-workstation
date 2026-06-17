mod router;
mod stdio;

use crate::workspace;
use anyhow::{Result, bail};
use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct RuntimeDaemonArgs {
    /// Run the daemon over line-delimited JSON-RPC on stdin/stdout.
    #[arg(long)]
    stdio: bool,
    #[command(flatten)]
    serve: workspace::ServeArgs,
}

pub(crate) fn run(args: RuntimeDaemonArgs) -> Result<()> {
    if !args.stdio {
        bail!("daemon currently supports only --stdio");
    }

    stdio::run_stdio(args.serve)
}

#[cfg(test)]
mod tests;
