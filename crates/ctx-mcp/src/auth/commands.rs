use super::{DEFAULT_BASE_URL, PROVIDER_ID, XaiProviderState, resolve_runtime_credentials};
use super::{oauth::run_loopback_login, store, util};
use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};
use std::path::Path;

#[derive(Debug, Args)]
pub(crate) struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// Sign in to xAI Grok OAuth (SuperGrok / Premium+) with browser PKCE.
    Login(LoginArgs),
    /// Show stored xAI OAuth status without printing secrets.
    Status(StatusArgs),
    /// Remove stored xAI OAuth credentials.
    Logout(LogoutArgs),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(super) enum AuthProvider {
    /// xAI Grok OAuth via browser PKCE.
    Xai,
}

#[derive(Debug, Args)]
pub(super) struct LoginArgs {
    /// Provider to login. Only `xai` is supported.
    #[arg(value_enum)]
    pub(super) provider: Option<AuthProvider>,
    /// Start a new browser OAuth flow even if stored credentials exist.
    #[arg(long)]
    pub(super) force: bool,
    /// Print the authorization URL but do not try to open a browser.
    #[arg(long = "no-browser")]
    pub(super) no_browser: bool,
    /// Skip the local listener and paste the callback URL/code manually.
    #[arg(long = "manual-paste")]
    pub(super) manual_paste: bool,
    /// OAuth login timeout in seconds.
    #[arg(long = "timeout", default_value_t = 120)]
    pub(super) timeout_seconds: u64,
}

#[derive(Debug, Args)]
struct StatusArgs {
    /// Refresh the token if it is expiring before printing status.
    #[arg(long)]
    refresh: bool,
}

#[derive(Debug, Args)]
struct LogoutArgs {
    /// Provider to logout. Only `xai` is supported.
    #[arg(value_enum)]
    provider: Option<AuthProvider>,
}

pub(super) fn run(args: AuthArgs) -> Result<()> {
    match args.command {
        AuthCommand::Login(login_args) => login(login_args),
        AuthCommand::Status(status_args) => status(status_args),
        AuthCommand::Logout(logout_args) => logout(logout_args),
    }
}

fn login(args: LoginArgs) -> Result<()> {
    ensure_provider(args.provider);
    if !args.force && try_reuse_existing()? {
        return Ok(());
    }

    println!("Signing in to xAI Grok OAuth (SuperGrok / Premium+)...");
    println!("Auth state: {}", store::auth_file_path()?.display());
    println!();

    let credentials = run_loopback_login(&args)?;
    let base_url = credentials.base_url.clone();
    let state = XaiProviderState {
        tokens: Some(credentials.tokens),
        discovery: Some(credentials.discovery),
        redirect_uri: Some(credentials.redirect_uri),
        base_url: Some(base_url.clone()),
        auth_mode: Some("oauth_pkce".to_string()),
        source: Some("oauth-loopback".to_string()),
        last_refresh_unix: Some(util::now_unix()),
        last_auth_error: None,
    };
    store::save_xai_state(state)?;
    println!();
    println!("Login successful.");
    println!("provider: {PROVIDER_ID}");
    println!("base_url: {base_url}");
    Ok(())
}

fn status(args: StatusArgs) -> Result<()> {
    if args.refresh {
        let credentials = resolve_runtime_credentials(true)?;
        print_status_authenticated(
            &store::auth_file_path()?,
            &credentials.base_url,
            &credentials.access_token,
            credentials.last_refresh_unix,
        );
        return Ok(());
    }

    let path = store::auth_file_path()?;
    let state = store::load_xai_state()?;
    let Some(state) = state else {
        println!("provider: {PROVIDER_ID}");
        println!("auth: {}", path.display());
        println!("status: not_logged_in");
        return Ok(());
    };
    let Some(tokens) = state.tokens else {
        println!("provider: {PROVIDER_ID}");
        println!("auth: {}", path.display());
        println!("status: invalid");
        return Ok(());
    };
    let base_url = state
        .base_url
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    print_status_authenticated(
        &path,
        &base_url,
        &tokens.access_token,
        state.last_refresh_unix,
    );
    Ok(())
}

fn logout(args: LogoutArgs) -> Result<()> {
    ensure_provider(args.provider);
    let path = store::auth_file_path()?;
    let _lock = store::acquire_auth_lock(&path)?;
    let mut store = store::load_store(&path)?;
    if store.providers.remove(PROVIDER_ID).is_some() {
        store::delete_xai_keyring_tokens(&path);
        super::store::save_store(&path, &store)?;
        println!("Removed xAI OAuth credentials from {}", path.display());
    } else {
        println!("No xAI OAuth credentials found at {}", path.display());
    }
    Ok(())
}

fn ensure_provider(provider: Option<AuthProvider>) {
    let _ = provider.unwrap_or(AuthProvider::Xai);
}

fn try_reuse_existing() -> Result<bool> {
    let Some(state) = store::load_xai_state()? else {
        return Ok(false);
    };
    let Some(tokens) = state.tokens else {
        return Ok(false);
    };
    if util::access_token_is_expiring(&tokens.access_token, 60) {
        match resolve_runtime_credentials(true) {
            Ok(credentials) => {
                println!("Existing xAI OAuth credentials refreshed.");
                println!("provider: {PROVIDER_ID}");
                println!("base_url: {}", credentials.base_url);
                return Ok(true);
            }
            Err(err) => {
                eprintln!("Stored xAI OAuth credentials could not be refreshed: {err}");
                eprintln!("Starting a new login flow. Use --force to skip reuse checks.");
                return Ok(false);
            }
        }
    }
    println!("Existing xAI OAuth credentials found.");
    println!("Use `ctx-mcp auth login xai --force` to sign in again.");
    Ok(true)
}

fn print_status_authenticated(
    path: &Path,
    base_url: &str,
    access_token: &str,
    last_refresh: Option<u64>,
) {
    println!("provider: {PROVIDER_ID}");
    println!("auth: {}", path.display());
    println!("status: authenticated");
    println!("base_url: {base_url}");
    match util::jwt_expiry(access_token) {
        Some(exp) => println!(
            "access_token: present (expires_unix: {exp}, {})",
            util::expiry_label(exp)
        ),
        None => println!("access_token: present"),
    }
    println!("refresh_token: present");
    if let Some(value) = last_refresh {
        println!("last_refresh_unix: {value}");
    }
}
