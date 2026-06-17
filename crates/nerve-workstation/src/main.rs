//! Stdio JSON-RPC server and small CLI for the context engine.

mod auth;
mod cli;
mod commands;
mod daemon;
mod jobs;
mod rpc;
mod server;
mod tools;
mod workspace;
mod xai;

fn main() -> anyhow::Result<()> {
    cli::run()
}
