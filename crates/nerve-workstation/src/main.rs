//! Stdio JSON-RPC server and small CLI for the context engine.

mod agent;
mod auth;
mod capabilities;
mod cli;
mod commands;
mod daemon;
mod hooks;
mod jobs;
mod mcp;
mod policy;
mod providers;
mod rpc;
mod server;
mod session;
mod tools;
mod workspace;
mod xai;

fn main() -> anyhow::Result<()> {
    cli::run()
}
