use crate::workspace::ServeArgs;
use crate::{auth, commands, daemon, server};
use anyhow::Result;
use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "ctx-mcp",
    version,
    about = "Minimal snapshot-centered context engine"
)]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    /// Run a synchronous JSON-RPC stdio MCP-like server.
    Serve(ServeArgs),
    /// Run the local AI Workstation Runtime daemon.
    #[command(name = "ctxd")]
    Daemon(daemon::RuntimeDaemonArgs),
    /// Print local toolchain diagnostics.
    Doctor,
    /// Inspect configuration.
    Config(ConfigArgs),
    /// Warm the current project's semantic index cache.
    Warm(ServeArgs),
    /// Manage xAI OAuth credentials.
    Auth(auth::AuthArgs),
    /// Manage local caches.
    Cache(CacheArgs),
    /// Register ctx-mcp as an MCP server in Claude Code and/or Codex.
    Install(commands::install::InstallArgs),
}

#[derive(Debug, Args)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Show canonical roots that would be allowed.
    Roots(ServeArgs),
}

#[derive(Debug, Args)]
struct CacheArgs {
    #[command(subcommand)]
    command: CacheCommand,
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    /// Delete the current project's semantic index cache.
    Purge(ServeArgs),
}

pub(crate) fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        CommandKind::Serve(args) => server::serve(args),
        CommandKind::Daemon(args) => daemon::run(args),
        CommandKind::Doctor => commands::doctor::doctor(),
        CommandKind::Config(args) => match args.command {
            ConfigCommand::Roots(serve_args) => commands::config::config_roots(serve_args),
        },
        CommandKind::Warm(args) => commands::cache::warm(args),
        CommandKind::Auth(args) => auth::run(args),
        CommandKind::Cache(args) => match args.command {
            CacheCommand::Purge(serve_args) => commands::cache::purge(serve_args),
        },
        CommandKind::Install(args) => commands::install::install(args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_warm_cache_and_auth() {
        let ctxd =
            Cli::try_parse_from(["ctx-mcp", "ctxd", "--stdio", "--root", "."]).expect("ctxd parse");
        assert!(matches!(ctxd.command, CommandKind::Daemon(_)));
        let warm = Cli::try_parse_from(["ctx-mcp", "warm"]).expect("warm parse");
        assert!(matches!(warm.command, CommandKind::Warm(_)));
        let purge = Cli::try_parse_from(["ctx-mcp", "cache", "purge"]).expect("purge parse");
        assert!(matches!(purge.command, CommandKind::Cache(_)));
        let login =
            Cli::try_parse_from(["ctx-mcp", "auth", "login", "xai"]).expect("auth login parse");
        assert!(matches!(login.command, CommandKind::Auth(_)));
        let status = Cli::try_parse_from(["ctx-mcp", "auth", "status"]).expect("status parse");
        assert!(matches!(status.command, CommandKind::Auth(_)));
        let logout = Cli::try_parse_from(["ctx-mcp", "auth", "logout"]).expect("logout parse");
        assert!(matches!(logout.command, CommandKind::Auth(_)));
    }
}
