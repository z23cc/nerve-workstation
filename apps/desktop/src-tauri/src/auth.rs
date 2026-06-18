//! Desktop-native OAuth login capture for the daemon's stateless `auth.*` protocol.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tauri::menu::{MenuBuilder, MenuEvent, SubmenuBuilder};
use tauri::{AppHandle, Manager};
use url::Url;

use crate::DaemonEndpointState;

const PROVIDERS: [(&str, &str); 3] = [
    ("anthropic", "Anthropic"),
    ("openai", "OpenAI"),
    ("xai", "xAI"),
];
const LOGIN_TIMEOUT: Duration = Duration::from_secs(180);
const JOB_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(120);
const STATUS_ITEM_ID: &str = "auth-status";
const LOGIN_PREFIX: &str = "auth-login-";

static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct LoginStart {
    login_id: String,
    authorize_url: String,
    redirect_uri: String,
}

pub fn install_menu(app: &mut tauri::App) -> tauri::Result<()> {
    let handle = app.handle();
    let mut auth_menu = SubmenuBuilder::new(handle, "Login")
        .text(STATUS_ITEM_ID, "Status: ready")
        .separator();
    for (id, label) in PROVIDERS {
        auth_menu = auth_menu.text(format!("{LOGIN_PREFIX}{id}"), format!("Sign in to {label}"));
    }
    let menu = MenuBuilder::new(handle).item(&auth_menu.build()?).build()?;
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| handle_menu_event(app, &event));
    Ok(())
}

fn handle_menu_event(app: &AppHandle, event: &MenuEvent) {
    let id = event.id().0.as_str();
    let Some(provider) = id.strip_prefix(LOGIN_PREFIX) else {
        return;
    };
    let provider = provider.to_string();
    let app = app.clone();
    std::thread::spawn(move || run_login(app, provider));
}

fn run_login(app: AppHandle, provider: String) {
    set_status(&app, &format!("Starting {provider} login…"));
    let result = login(&app, &provider);
    match result {
        Ok(status) => set_status(&app, &format!("{provider} login complete: {status}")),
        Err(err) => set_status(&app, &format!("{provider} login failed: {err}")),
    }
}

fn login(app: &AppHandle, provider: &str) -> Result<String, String> {
    let daemon = daemon_url(app)?;
    let start = auth_start(&daemon, provider)?;
    let listener = bind_callback_listener(&start.redirect_uri)?;
    open_browser(&start.authorize_url)?;
    set_status(app, "Waiting for browser callback…");
    let callback_url = capture_callback(&listener, &start.redirect_uri, LOGIN_TIMEOUT)?;
    set_status(app, "Completing login with daemon…");
    let result = auth_complete(&daemon, &start.login_id, &callback_url)?;
    Ok(credential_status(&result))
}

fn daemon_url(app: &AppHandle) -> Result<Url, String> {
    let raw = app
        .state::<DaemonEndpointState>()
        .0
        .lock()
        .map_err(|_| "daemon endpoint lock poisoned".to_string())?
        .clone()
        .ok_or_else(|| "daemon URL is not ready yet".to_string())?;
    Url::parse(&raw).map_err(|err| format!("invalid daemon URL `{raw}`: {err}"))
}

fn auth_start(daemon: &Url, provider: &str) -> Result<LoginStart, String> {
    let result = run_command_job(
        daemon,
        json!({ "kind": "auth.start", "provider": provider }),
    )?;
    Ok(LoginStart {
        login_id: required_string(&result, "login_id")?,
        authorize_url: required_string(&result, "authorize_url")?,
        redirect_uri: required_string(&result, "redirect_uri")?,
    })
}

fn auth_complete(daemon: &Url, login_id: &str, callback_url: &str) -> Result<Value, String> {
    run_command_job(
        daemon,
        json!({
            "kind": "auth.complete",
            "login_id": login_id,
            "callback_url": callback_url,
        }),
    )
}

fn run_command_job(daemon: &Url, command: Value) -> Result<Value, String> {
    let seq = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
    let job_id = format!("desktop-auth-{}-{seq}", now_millis());
    rpc(
        daemon,
        "runtime/jobs/start",
        json!({ "job_id": job_id, "command": command }),
    )?;
    wait_job_result(daemon, &job_id)
}

fn wait_job_result(daemon: &Url, job_id: &str) -> Result<Value, String> {
    let deadline = Instant::now() + JOB_TIMEOUT;
    loop {
        let result = rpc(
            daemon,
            "runtime/jobs/get",
            json!({ "job_id": job_id, "include_result": true }),
        )?;
        let job = result.get("job").ok_or("job response missing `job`")?;
        match job.get("status").and_then(Value::as_str) {
            Some("completed") => return Ok(job.get("result").cloned().unwrap_or(Value::Null)),
            Some("failed") => return Err(job_error(job)),
            Some("cancelled") => return Err("auth job was cancelled".to_string()),
            Some(_) if Instant::now() < deadline => std::thread::sleep(POLL_INTERVAL),
            Some(status) => return Err(format!("auth job timed out while {status}")),
            None => return Err("job response missing `status`".to_string()),
        }
    }
}

fn rpc(daemon: &Url, method: &str, params: Value) -> Result<Value, String> {
    let mut rpc_url = daemon.clone();
    rpc_url.set_path("/rpc");
    rpc_url.set_query(None);
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let response = http_post_json(&rpc_url, &body)?;
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("rpc error");
        return Err(message.to_string());
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

fn http_post_json(url: &Url, value: &Value) -> Result<Value, String> {
    if url.scheme() != "http" {
        return Err("desktop auth currently supports daemon http:// URLs".to_string());
    }
    let host = url.host_str().ok_or("daemon URL missing host")?;
    let port = url
        .port_or_known_default()
        .ok_or("daemon URL missing port")?;
    let body = serde_json::to_vec(value).map_err(|err| err.to_string())?;
    let mut stream =
        TcpStream::connect((host, port)).map_err(|err| format!("connect {host}:{port}: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|err| err.to_string())?;
    write_http_request(&mut stream, url, host, &body)?;
    read_http_response(stream)
}

fn write_http_request(
    stream: &mut TcpStream,
    url: &Url,
    host: &str,
    body: &[u8],
) -> Result<(), String> {
    let target = match url.query() {
        Some(query) => format!("{}?{}", url.path(), query),
        None => url.path().to_string(),
    };
    write!(
        stream,
        "POST {target} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .map_err(|err| err.to_string())?;
    stream.write_all(body).map_err(|err| err.to_string())
}

fn read_http_response(mut stream: TcpStream) -> Result<Value, String> {
    let mut bytes = Vec::new();
    stream
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    let response = String::from_utf8(bytes).map_err(|err| err.to_string())?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or("invalid daemon HTTP response")?;
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return Err(head.lines().next().unwrap_or("HTTP error").to_string());
    }
    serde_json::from_str(body).map_err(|err| format!("decode daemon JSON response: {err}"))
}

fn bind_callback_listener(redirect_uri: &str) -> Result<TcpListener, String> {
    let url = loopback_redirect_url(redirect_uri)?;
    let host = url.host_str().ok_or("redirect_uri missing host")?;
    let port = url
        .port_or_known_default()
        .ok_or("redirect_uri missing port")?;
    let listener =
        TcpListener::bind((host, port)).map_err(|err| format!("bind {host}:{port}: {err}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|err| format!("configure callback listener: {err}"))?;
    Ok(listener)
}

fn capture_callback(
    listener: &TcpListener,
    redirect_uri: &str,
    timeout: Duration,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                if let Some(callback) = handle_callback(&mut stream, redirect_uri)? {
                    return Ok(callback);
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                return Err("timed out waiting for OAuth callback".to_string());
            }
            Err(err) => return Err(format!("accept OAuth callback: {err}")),
        }
    }
}

fn handle_callback(stream: &mut TcpStream, redirect_uri: &str) -> Result<Option<String>, String> {
    let mut buf = [0; 4096];
    let n = stream.read(&mut buf).map_err(|err| err.to_string())?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let callback = request_target(&request).and_then(|target| callback_url(redirect_uri, &target));
    let ok = callback
        .as_deref()
        .map(|url| {
            callback_matches_redirect(redirect_uri, url).unwrap_or(false)
                && callback_has_code(url).unwrap_or(false)
        })
        .unwrap_or(false);
    write_callback_response(stream, ok)?;
    Ok(ok.then(|| callback.expect("callback is present when ok")))
}

fn request_target(request: &str) -> Result<String, String> {
    let line = request
        .lines()
        .next()
        .ok_or("empty OAuth callback request")?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    if method != "GET" || target.is_empty() {
        return Err("invalid OAuth callback request".to_string());
    }
    Ok(target.to_string())
}

fn loopback_redirect_url(redirect_uri: &str) -> Result<Url, String> {
    let url = Url::parse(redirect_uri).map_err(|err| format!("invalid redirect_uri: {err}"))?;
    if url.scheme() != "http" {
        return Err("OAuth redirect_uri must use http:// loopback".to_string());
    }
    let host = url.host_str().ok_or("redirect_uri missing host")?;
    if !matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") {
        return Err(format!("OAuth redirect_uri host is not loopback: {host}"));
    }
    Ok(url)
}

fn callback_matches_redirect(redirect_uri: &str, callback: &str) -> Result<bool, String> {
    let redirect = loopback_redirect_url(redirect_uri)?;
    let callback = Url::parse(callback).map_err(|err| format!("invalid callback URL: {err}"))?;
    Ok(callback.scheme() == redirect.scheme()
        && callback.host_str() == redirect.host_str()
        && callback.port_or_known_default() == redirect.port_or_known_default()
        && callback.path() == redirect.path())
}

fn callback_has_code(callback: &str) -> Result<bool, String> {
    let callback = Url::parse(callback).map_err(|err| format!("invalid callback URL: {err}"))?;
    Ok(callback
        .query_pairs()
        .any(|(key, value)| key == "code" && !value.is_empty()))
}

fn callback_url(redirect_uri: &str, target: &str) -> Result<String, String> {
    let redirect = loopback_redirect_url(redirect_uri)?;
    let mut base = format!(
        "{}://{}",
        redirect.scheme(),
        redirect.host_str().unwrap_or("localhost")
    );
    if let Some(port) = redirect.port() {
        base.push_str(&format!(":{port}"));
    }
    Ok(format!("{base}{target}"))
}

fn write_callback_response(stream: &mut TcpStream, ok: bool) -> Result<(), String> {
    let body = if ok {
        "<html><body><h1>Login complete</h1><p>You can close this tab and return to Nerve Workstation.</p></body></html>"
    } else {
        "<html><body><h1>Login failed</h1><p>The OAuth callback did not include a code.</p></body></html>"
    };
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .map_err(|err| err.to_string())
}

fn validate_external_url(raw: &str) -> Result<(), String> {
    let url = Url::parse(raw).map_err(|err| format!("invalid authorize_url: {err}"))?;
    match url.scheme() {
        "http" | "https" => Ok(()),
        scheme => Err(format!(
            "refusing to open non-web authorize_url scheme: {scheme}"
        )),
    }
}

fn open_browser(url: &str) -> Result<(), String> {
    validate_external_url(url)?;
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.arg(url);
        command
    } else if cfg!(target_os = "windows") {
        let mut command = Command::new("rundll32");
        command.args(["url.dll,FileProtocolHandler", url]);
        command
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .spawn()
        .map_err(|err| format!("open browser: {err}"))?;
    Ok(())
}

fn set_status(app: &AppHandle, message: &str) {
    if let Some(item) = app.menu().and_then(|menu| menu.get(STATUS_ITEM_ID)) {
        if let Some(item) = item.as_menuitem() {
            let _ = item.set_text(format!("Status: {message}"));
        }
    }
    show_status_overlay(app, message);
}

fn show_status_overlay(app: &AppHandle, message: &str) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let Ok(message) = serde_json::to_string(message) else {
        return;
    };
    let script = format!(
        r#"(() => {{
          let el = document.getElementById("nerveDesktopAuthStatus");
          if (!el) {{
            el = document.createElement("div");
            el.id = "nerveDesktopAuthStatus";
            el.style.cssText = "position:fixed;right:16px;bottom:16px;z-index:2147483647;max-width:420px;padding:10px 12px;border-radius:10px;background:#111827;color:#f9fafb;font:13px system-ui;box-shadow:0 8px 30px rgba(0,0,0,.35)";
            document.body.appendChild(el);
          }}
          el.textContent = {message};
        }})()"#
    );
    let _ = window.eval(script);
}

fn required_string(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("auth response missing `{key}`"))
}

fn credential_status(value: &Value) -> String {
    value
        .get("credential")
        .unwrap_or(value)
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("authenticated")
        .to_string()
}

fn job_error(job: &Value) -> String {
    job.get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("auth job failed")
        .to_string()
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}
