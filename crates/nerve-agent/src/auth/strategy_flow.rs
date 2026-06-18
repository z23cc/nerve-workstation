use super::oauth::announce_and_open;
use super::*;

pub(super) fn default_redirect_uri(loopback: &Loopback) -> String {
    format!(
        "http://{}:{}{}",
        loopback.host, loopback.port, loopback.path
    )
}

pub(super) fn make_login_start(
    provider: ProviderId,
    redirect_uri: &str,
    build_authorize_url: impl FnOnce(&str, &str, &Pkce) -> AgentResult<String>,
) -> AgentResult<LoginStart> {
    let pkce = Pkce::generate();
    let state = oauth::random_urlsafe(24);
    let authorize_url = build_authorize_url(redirect_uri, &state, &pkce)?;
    Ok(LoginStart {
        provider,
        authorize_url,
        redirect_uri: redirect_uri.to_string(),
        state,
        verifier: pkce.verifier,
        provider_data: Value::Null,
    })
}

/// Run the shared loopback/manual-paste dance and complete the staged login.
pub(super) fn run_interactive_login(
    strategy: &dyn AuthStrategy,
    loopback: &Loopback,
    opts: &LoginOptions,
) -> AgentResult<Credential> {
    let server = if opts.manual_paste {
        None
    } else {
        Some(oauth::start_loopback_server(
            loopback.host,
            loopback.port,
            loopback.path,
            loopback.allow_fallback,
        )?)
    };
    let redirect_uri = server.as_ref().map_or_else(
        || default_redirect_uri(loopback),
        |server| server.redirect_uri.clone(),
    );
    let start = strategy.start(&redirect_uri)?;
    announce_and_open(&start.authorize_url, opts.no_browser || opts.manual_paste);

    let callback = match &server {
        Some(server) => {
            println!();
            println!("Waiting for callback on {redirect_uri}");
            oauth::wait_for_callback(
                server,
                loopback.path,
                opts.timeout,
                &start.state,
                &opts.cancel,
            )?
        }
        None => oauth::prompt_manual_callback()?,
    };
    strategy.complete(&start, &callback, &opts.cancel)
}

pub(super) fn ensure_login_provider(start: &LoginStart, provider: ProviderId) -> AgentResult<()> {
    if start.provider == provider {
        Ok(())
    } else {
        Err(AgentError::Auth(format!(
            "login was started for {}, not {}",
            start.provider.as_str(),
            provider.as_str()
        )))
    }
}

pub(super) fn validated_code(
    start: &LoginStart,
    callback: &oauth::OAuthCallback,
) -> AgentResult<String> {
    oauth::validate_callback(callback, &start.state)?;
    oauth::require_code(callback)
}
