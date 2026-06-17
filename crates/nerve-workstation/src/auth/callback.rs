use super::*;
use std::net::TcpListener;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use url::form_urlencoded;

pub(super) struct OAuthCallback {
    pub(super) code: Option<String>,
    pub(super) state: Option<String>,
    pub(super) error: Option<String>,
    pub(super) error_description: Option<String>,
    pub(super) manual_paste: bool,
}

pub(super) struct LoopbackServer {
    server: Server,
    pub(super) redirect_uri: String,
}

pub(super) fn start_loopback_server() -> Result<LoopbackServer> {
    let listener = TcpListener::bind((REDIRECT_HOST, REDIRECT_PORT))
        .or_else(|_| TcpListener::bind((REDIRECT_HOST, 0)))
        .context("failed to bind xAI OAuth loopback listener")?;
    let port = listener.local_addr()?.port();
    let server = Server::from_listener(listener, None)
        .map_err(|err| anyhow!("failed to start xAI OAuth loopback server: {err}"))?;
    Ok(LoopbackServer {
        server,
        redirect_uri: format!("http://{REDIRECT_HOST}:{port}{REDIRECT_PATH}"),
    })
}

pub(super) fn wait_for_callback(
    server: LoopbackServer,
    timeout: Duration,
    expected_state: &str,
) -> Result<OAuthCallback> {
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            bail!("timed out waiting for xAI OAuth callback");
        }
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(250));
        match server.server.recv_timeout(wait) {
            Ok(Some(request)) => {
                if let Some(callback) = handle_callback_request(request, expected_state)? {
                    return Ok(callback);
                }
            }
            Ok(None) => {}
            Err(err) => return Err(err).context("failed while waiting for xAI OAuth callback"),
        }
    }
}

pub(super) fn handle_callback_request(
    request: Request,
    expected_state: &str,
) -> Result<Option<OAuthCallback>> {
    let method = request.method().clone();
    let target = request.url().to_string();
    if method == Method::Options {
        respond_http(request, 204, "")?;
        return Ok(None);
    }
    if method != Method::Get {
        respond_http(request, 405, "method not allowed")?;
        return Ok(None);
    }
    if target.split('?').next() != Some(REDIRECT_PATH) {
        respond_http(request, 404, "not found")?;
        return Ok(None);
    }
    if !target.contains('?') {
        respond_http(request, 400, "OAuth callback missing query parameters")?;
        return Ok(None);
    }
    let callback = parse_callback_target(&target, false)?;
    let terminal = callback.error.is_some() || callback.code.is_some();
    if !terminal {
        respond_http(request, 400, "OAuth callback missing code or error")?;
        return Ok(None);
    }
    if callback.state.as_deref() != Some(expected_state) {
        respond_http(request, 400, "OAuth callback state mismatch")?;
        return Ok(None);
    }
    respond_http(request, 200, "Nerve login complete; return to the terminal")?;
    Ok(Some(callback))
}

pub(super) fn respond_http(request: Request, status: u16, message: &str) -> Result<()> {
    let body = if status == 204 {
        String::new()
    } else {
        format!("<html><body><p>{message}</p></body></html>")
    };
    let mut response = Response::from_string(body).with_status_code(StatusCode(status));
    for (name, value) in [
        ("Content-Type", "text/html; charset=utf-8"),
        ("Access-Control-Allow-Origin", "*"),
        ("Access-Control-Allow-Methods", "GET, OPTIONS"),
        ("Access-Control-Allow-Headers", "*"),
        ("Access-Control-Allow-Private-Network", "true"),
        ("Connection", "close"),
    ] {
        response.add_header(static_header(name, value));
    }
    request
        .respond(response)
        .context("failed to write OAuth callback response")
}

fn static_header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("static HTTP header is valid")
}

pub(super) fn prompt_manual_callback() -> Result<OAuthCallback> {
    println!();
    println!("Paste the full callback URL or authorization code, then press Enter:");
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read callback URL")?;
    parse_pasted_callback(input.trim())
}

pub(super) fn parse_pasted_callback(input: &str) -> Result<OAuthCallback> {
    if input.contains("code=") || input.contains("error=") {
        let target = input
            .split_once(REDIRECT_HOST)
            .map_or(input, |(_, tail)| tail);
        let path = target.find('/').map_or(target, |idx| &target[idx..]);
        return parse_callback_target(path, true);
    }
    Ok(OAuthCallback {
        code: Some(input.trim().to_string()),
        state: None,
        error: None,
        error_description: None,
        manual_paste: true,
    })
}

pub(super) fn parse_callback_target(target: &str, manual_paste: bool) -> Result<OAuthCallback> {
    let (_, query) = target
        .split_once('?')
        .ok_or_else(|| anyhow!("OAuth callback did not include query parameters"))?;
    let params = parse_query(query)?;
    Ok(OAuthCallback {
        code: params.get("code").cloned(),
        state: params.get("state").cloned(),
        error: params.get("error").cloned(),
        error_description: params.get("error_description").cloned(),
        manual_paste,
    })
}

pub(super) fn validate_callback(callback: &OAuthCallback, expected_state: &str) -> Result<()> {
    if let Some(error) = &callback.error {
        let description = callback.error_description.as_deref().unwrap_or(error);
        bail!("xAI authorization failed: {description}");
    }
    if callback.state.as_deref() == Some(expected_state) {
        return Ok(());
    }
    if callback.manual_paste && callback.state.is_none() {
        return Ok(());
    }
    bail!("xAI authorization failed: state mismatch")
}

pub(super) fn parse_query(query: &str) -> Result<BTreeMap<String, String>> {
    Ok(form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect())
}

pub(super) fn try_open_browser(url: &str) {
    if open::that(url).is_err() {
        println!("Could not open the browser automatically; use the URL above.");
    }
}
