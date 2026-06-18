//! `nerve auth` — an xAI-only alias over [`nerve_agent::auth`].
//!
//! Kept for compatibility; `nerve agent login --provider xai` is the general
//! entry point and shares the same credential store. Login/status/logout here
//! operate solely on the xAI provider.

use anyhow::{Result, anyhow};
use clap::{Args, Subcommand, ValueEnum};
use nerve_agent::auth::{self, AuthMode, LoginOptions, ProviderId};

#[derive(Debug, Args)]
pub(crate) struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// Sign in to xAI Grok OAuth (SuperGrok / Premium+) with browser PKCE.
    Login(LoginArgs),
    /// Show stored xAI credential status without printing secrets.
    Status,
    /// Remove stored xAI credentials.
    Logout(LogoutArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AuthProvider {
    /// xAI Grok OAuth via browser PKCE.
    Xai,
}

#[derive(Debug, Args)]
struct LoginArgs {
    /// Provider to login. Only `xai` is supported here.
    #[arg(value_enum)]
    provider: Option<AuthProvider>,
    /// Start a new browser OAuth flow even if stored credentials exist.
    #[arg(long)]
    force: bool,
    /// Print the authorization URL but do not open a browser.
    #[arg(long = "no-browser")]
    no_browser: bool,
    /// Skip the loopback listener and paste the callback URL manually.
    #[arg(long = "manual-paste")]
    manual_paste: bool,
    /// OAuth login timeout in seconds.
    #[arg(long = "timeout", default_value_t = 120)]
    timeout_seconds: u64,
}

#[derive(Debug, Args)]
struct LogoutArgs {
    /// Provider to logout. Only `xai` is supported here.
    #[arg(value_enum)]
    provider: Option<AuthProvider>,
}

pub(crate) fn run(args: AuthArgs) -> Result<()> {
    match args.command {
        AuthCommand::Login(login_args) => login(login_args),
        AuthCommand::Status => status(),
        AuthCommand::Logout(logout_args) => logout(logout_args),
    }
}

fn login(args: LoginArgs) -> Result<()> {
    ensure_xai(args.provider);
    if !args.force
        && let Some(credential) = auth::load_credential(ProviderId::Xai)
            .map_err(|err| anyhow!("failed to read stored credentials: {err}"))?
    {
        println!(
            "Existing xAI credentials found ({}). Use `nerve auth login xai --force` to sign in again.",
            mode_label(credential.mode)
        );
        return Ok(());
    }

    println!("Signing in to xAI Grok OAuth (SuperGrok / Premium+)...");
    let opts = LoginOptions {
        no_browser: args.no_browser,
        manual_paste: args.manual_paste,
        timeout: std::time::Duration::from_secs(args.timeout_seconds),
    };
    let credential = auth::strategy_for(ProviderId::Xai)
        .login(&opts)
        .map_err(|err| anyhow!("xAI login failed: {err}"))?;
    auth::save_credential(&credential)
        .map_err(|err| anyhow!("failed to store credential: {err}"))?;
    println!("Login successful. base_url: {}", credential.base_url);
    Ok(())
}

fn status() -> Result<()> {
    match auth::load_credential(ProviderId::Xai)
        .map_err(|err| anyhow!("failed to read credentials: {err}"))?
    {
        None => println!("provider: xai\nstatus: not_logged_in"),
        Some(credential) => {
            println!("provider: xai");
            println!("status: authenticated ({})", mode_label(credential.mode));
            println!("base_url: {}", credential.base_url);
            match credential.expires_at_unix {
                Some(exp) => println!("access_token: present (expires_unix: {exp})"),
                None => println!("access_token: present"),
            }
        }
    }
    Ok(())
}

fn logout(args: LogoutArgs) -> Result<()> {
    ensure_xai(args.provider);
    auth::delete_credential(ProviderId::Xai)
        .map_err(|err| anyhow!("failed to remove credentials: {err}"))?;
    println!("Removed stored xAI credentials.");
    Ok(())
}

fn ensure_xai(provider: Option<AuthProvider>) {
    // Only xAI is supported by this alias; `nerve agent` handles other providers.
    let _ = provider.unwrap_or(AuthProvider::Xai);
}

fn mode_label(mode: AuthMode) -> &'static str {
    match mode {
        AuthMode::Oauth => "oauth",
        AuthMode::ApiKey => "api key",
    }
}
