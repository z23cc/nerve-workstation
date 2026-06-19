//! HTTP transport for the runtime daemon — a *new transport for the existing
//! protocol*, never a new protocol (architecture north star §3/§6/§8: "a new
//! client surface reuses the versioned runtime protocol, never a bespoke RPC";
//! "the GUI transport reuses the transport-neutral router"). It adds **no**
//! `RuntimeCommand` / `RuntimeEvent` / method — it drives the same
//! [`RuntimeDaemonRouter`] the stdio transport does, so a browser GUI speaks
//! the identical Protocol v3.
//!
//! Endpoints (bound to loopback by default):
//!   * `POST /rpc`    — one JSON-RPC request in, its JSON-RPC response out.
//!   * `GET  /events` — Server-Sent Events stream of `runtime/event` notifications.
//!   * `GET  /`       — the embedded single-page GUI ([`GUI_HTML`], `gui.html`),
//!     a client of this same Protocol v3 (it only ever talks to `/rpc` + `/events`).
//!
//! Event fan-out: the router is handed a single notification sink that feeds an
//! [`SseHub`]; every open `/events` connection registers an mpsc channel and
//! writes each notification as a `data: <json>\n\n` SSE frame.
//!
//! Transport hardening ([`HttpSecurity`]) — this is a *transport guard*, it adds
//! no protocol vocabulary:
//!   * **Bearer token.** A per-run [`random_urlsafe`] token gates `POST /rpc` and
//!     `GET /events` (else `401`). On a loopback bind it is embedded into the
//!     served GUI so the local UI stays zero-config; on a remote bind it is
//!     supplied out-of-band via the URL fragment (never sent to the server).
//!   * **Origin allowlist.** CORS headers are echoed only for loopback origins —
//!     never the old `*` — so a foreign web page cannot read daemon responses.
//!   * **Host guard.** A loopback bind also requires a loopback `Host` header,
//!     defeating DNS-rebinding. A non-loopback bind is refused unless the
//!     operator passes `--http-allow-remote`, which keeps the token mandatory.

use super::router::RuntimeDaemonRouter;
use crate::rpc::{RpcMessage, jsonrpc_error};
use crate::workspace;
use anyhow::{Result, anyhow};
use nerve_agent::auth::oauth::random_urlsafe;
use serde_json::Value;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

/// Idle interval after which an open SSE connection emits a keep-alive comment.
/// Also bounds how quickly a vanished client is noticed (its next write fails).
const SSE_HEARTBEAT: Duration = Duration::from_secs(15);

/// SSE keep-alive comment frame: ignored by `EventSource`, keeps the socket hot.
const SSE_KEEPALIVE: &str = ": keep-alive\n\n";

/// The self-contained runtime GUI, embedded into the binary so the daemon serves
/// it with no build step or external assets. It is a *client* of Protocol v3:
/// it only POSTs `/rpc` and subscribes to `/events`, adding no new transport.
const GUI_HTML: &str = include_str!("gui.html");

/// Placeholder in [`GUI_HTML`] replaced with the per-run bearer token on a
/// loopback bind. Kept distinct from its bare substring so [`HttpSecurity::render_gui`]
/// and the GUI's own bootstrap can both tell "injected" from "not injected".
const GUI_TOKEN_PLACEHOLDER: &str = "__NERVE_DAEMON_TOKEN__";

/// Run the daemon HTTP transport, blocking until the process exits.
pub(super) fn run_http(serve_args: workspace::ServeArgs, addr: SocketAddr) -> Result<()> {
    let security = HttpSecurity::new(addr);
    let hub = Arc::new(SseHub::new());
    let broadcast = Arc::clone(&hub);
    let router = super::setup::build_router(&serve_args, move |value| broadcast.broadcast(value))?;
    let ctx = Arc::new(HttpContext {
        router,
        hub,
        security,
    });
    let server = Server::http(addr)
        .map_err(|err| anyhow!("failed to bind daemon HTTP transport on {addr}: {err}"))?;
    eprintln!("nerve daemon: HTTP transport on http://{addr} (POST /rpc, GET /events)");
    announce_access(&ctx.security, addr);
    for request in server.incoming_requests() {
        let ctx = Arc::clone(&ctx);
        // Thread-per-request so long-lived `/events` streams never block the
        // accept loop or concurrent `/rpc` calls.
        std::thread::spawn(move || {
            if let Err(err) = handle_request(&ctx, request) {
                eprintln!("nerve daemon: HTTP request error: {err}");
            }
        });
    }
    Ok(())
}

/// Print the bearer token and how to reach the GUI. The token always goes to
/// stderr (the operator's own terminal) so non-browser clients can authenticate;
/// on a remote bind it is *not* embedded in the page, so the operator must open
/// the printed `#token=` URL.
fn announce_access(security: &HttpSecurity, addr: SocketAddr) {
    if security.enforce_loopback_host {
        eprintln!("nerve daemon: loopback-only; GUI token embedded at GET /");
    } else {
        eprintln!(
            "nerve daemon: REMOTE bind — token required; open http://{addr}/#token={}",
            security.token
        );
    }
    eprintln!("nerve daemon: auth token: {}", security.token);
}

/// Shared, thread-safe state handed to every request thread.
struct HttpContext {
    router: RuntimeDaemonRouter,
    hub: Arc<SseHub>,
    security: HttpSecurity,
}

/// Per-run transport hardening for the HTTP daemon. See the module docs.
struct HttpSecurity {
    /// Bearer token required on `/rpc` and `/events`, fresh each run.
    token: String,
    /// True for a loopback bind: also require a loopback `Host` header (DNS
    /// rebinding defense) and embed the token into the served GUI.
    enforce_loopback_host: bool,
}

impl HttpSecurity {
    fn new(addr: SocketAddr) -> Self {
        Self {
            token: random_urlsafe(32),
            enforce_loopback_host: addr.ip().is_loopback(),
        }
    }

    /// Whether a request carries the correct token, via `Authorization: Bearer`
    /// (used by `/rpc` fetch) or a `?token=` query (used by `/events`, since
    /// `EventSource` cannot set headers). Pure over its two inputs for testing.
    fn token_matches(&self, auth_header: Option<&str>, query: Option<&str>) -> bool {
        let provided = auth_header
            .and_then(bearer_from_header)
            .or_else(|| query.and_then(|q| query_param(q, "token")));
        provided.is_some_and(|tok| ct_eq(tok.as_bytes(), self.token.as_bytes()))
    }

    fn authorize(&self, request: &Request) -> bool {
        let auth = header_value(request, "Authorization");
        let query = request.url().split_once('?').map(|(_, q)| q);
        self.token_matches(auth, query)
    }

    /// The GUI to serve: a loopback bind embeds the token (zero-config local UI);
    /// a remote bind serves the template unchanged so the page never carries the
    /// token — the operator supplies it via the URL fragment.
    fn render_gui(&self, template: &str) -> String {
        if self.enforce_loopback_host {
            template.replace(GUI_TOKEN_PLACEHOLDER, &self.token)
        } else {
            template.to_string()
        }
    }
}

fn handle_request(ctx: &HttpContext, request: Request) -> Result<()> {
    // DNS-rebinding defense: a loopback bind only ever serves loopback Hosts.
    if ctx.security.enforce_loopback_host && !host_is_loopback(&request) {
        return respond_text(request, 403, "forbidden: non-loopback Host", None);
    }
    let cors = allowed_origin(&request);
    let method = request.method().clone();
    let path = request
        .url()
        .split('?')
        .next()
        .unwrap_or_default()
        .to_string();
    match (&method, path.as_str()) {
        (Method::Post, "/rpc") => guarded(ctx, request, cors.as_deref(), |req, cors| {
            handle_rpc(&ctx.router, req, cors)
        }),
        (Method::Get, "/events") => guarded(ctx, request, cors.as_deref(), |req, cors| {
            handle_events(&ctx.hub, req, cors)
        }),
        (Method::Options, _) => respond_preflight(request, cors.as_deref()),
        (Method::Get, "/") => {
            let html = ctx.security.render_gui(GUI_HTML);
            respond_html(request, &html, cors.as_deref())
        }
        _ => respond_text(request, 404, "not found", cors.as_deref()),
    }
}

/// Run `handler` only if the request carries the bearer token, else answer `401`.
/// Keeps the token check off the `GET /` (bootstrap) and `OPTIONS` (preflight)
/// paths, which must stay reachable for the GUI to load and CORS to negotiate.
fn guarded(
    ctx: &HttpContext,
    request: Request,
    cors: Option<&str>,
    handler: impl FnOnce(Request, Option<&str>) -> Result<()>,
) -> Result<()> {
    if ctx.security.authorize(&request) {
        handler(request, cors)
    } else {
        respond_text(request, 401, "unauthorized", cors)
    }
}

// ---- POST /rpc ------------------------------------------------------------

fn handle_rpc(
    router: &RuntimeDaemonRouter,
    mut request: Request,
    cors: Option<&str>,
) -> Result<()> {
    let mut body = String::new();
    request
        .as_reader()
        .read_to_string(&mut body)
        .map_err(|err| anyhow!("failed to read /rpc body: {err}"))?;
    match route_rpc_body(router, &body) {
        Some(response) => respond_json(request, 200, &response, cors),
        None => respond_text(request, 204, "", cors),
    }
}

/// Route one JSON-RPC request body through the shared router and return the
/// response to send (or `None` for a notification that produces no response).
/// Pure over `(router, body)`, hence unit-testable without a socket.
fn route_rpc_body(router: &RuntimeDaemonRouter, body: &str) -> Option<Value> {
    let message: RpcMessage = match serde_json::from_str(body) {
        Ok(message) => message,
        Err(err) => return Some(jsonrpc_error(Value::Null, -32700, err.to_string())),
    };
    let mut responses = Vec::new();
    if let Err(err) = router.handle_message(message, |value| {
        responses.push(value);
        Ok(())
    }) {
        return Some(jsonrpc_error(Value::Null, -32603, err.to_string()));
    }
    responses.into_iter().next()
}

// ---- GET /events (SSE) ----------------------------------------------------

fn handle_events(hub: &Arc<SseHub>, request: Request, cors: Option<&str>) -> Result<()> {
    let (id, receiver) = hub.subscribe();
    // The Origin echo is fixed for the life of the stream; copy it out before the
    // request is consumed by `into_writer`.
    let cors = cors.map(str::to_string);
    let mut writer = request.into_writer();
    let result = stream_events(&mut writer, &receiver, cors.as_deref());
    hub.unsubscribe(id);
    result
}

/// Write the SSE response head, then forward each broadcast notification as a
/// `data:` frame until the client disconnects (a failed write) or the hub stops.
fn stream_events(
    writer: &mut dyn Write,
    receiver: &Receiver<Arc<str>>,
    cors: Option<&str>,
) -> Result<()> {
    if write_sse_head(writer, cors).is_err() {
        return Ok(());
    }
    loop {
        let frame = match receiver.recv_timeout(SSE_HEARTBEAT) {
            Ok(json) => sse_data_frame(&json),
            Err(RecvTimeoutError::Timeout) => SSE_KEEPALIVE.to_string(),
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if write_sse_chunk(writer, frame.as_bytes()).is_err() {
            break; // client went away
        }
    }
    Ok(())
}

/// Format a runtime notification as an SSE `data:` event. The JSON is a single
/// line (serde never emits raw newlines), so one `data:` line is always valid.
fn sse_data_frame(json: &str) -> String {
    format!("data: {json}\n\n")
}

fn write_sse_head(writer: &mut dyn Write, cors: Option<&str>) -> std::io::Result<()> {
    writer.write_all(
        b"HTTP/1.1 200 OK\r\n\
          Content-Type: text/event-stream\r\n\
          Cache-Control: no-cache\r\n\
          Connection: keep-alive\r\n",
    )?;
    if let Some(origin) = cors {
        write!(
            writer,
            "Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\n"
        )?;
    }
    writer.write_all(b"Transfer-Encoding: chunked\r\n\r\n")?;
    writer.flush()
}

/// Write one HTTP/1.1 chunked-transfer chunk and flush, so the browser's
/// `EventSource` receives the frame immediately rather than buffered.
fn write_sse_chunk(writer: &mut dyn Write, payload: &[u8]) -> std::io::Result<()> {
    write!(writer, "{:X}\r\n", payload.len())?;
    writer.write_all(payload)?;
    writer.write_all(b"\r\n")?;
    writer.flush()
}

// ---- SSE broadcast hub ----------------------------------------------------

/// A minimal broadcast hub: the router's single notification sink fans out to
/// every open `/events` subscriber. It lives in the transport (not in
/// `nerve-runtime`) so the protocol vocabulary stays untouched.
struct SseHub {
    subscribers: Mutex<Vec<Subscriber>>,
    next_id: AtomicU64,
}

struct Subscriber {
    id: u64,
    sender: mpsc::Sender<Arc<str>>,
}

impl SseHub {
    fn new() -> Self {
        Self {
            subscribers: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Register a subscriber, returning its id and the receiver its `/events`
    /// connection drains.
    fn subscribe(&self) -> (u64, Receiver<Arc<str>>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        crate::sync::lock_recover(&self.subscribers).push(Subscriber { id, sender });
        (id, receiver)
    }

    fn unsubscribe(&self, id: u64) {
        crate::sync::lock_recover(&self.subscribers).retain(|subscriber| subscriber.id != id);
    }

    /// Fan a runtime notification out to all live subscribers, dropping any
    /// whose receiver has hung up.
    fn broadcast(&self, value: Value) {
        let frame: Arc<str> = Arc::from(value.to_string());
        crate::sync::lock_recover(&self.subscribers)
            .retain(|subscriber| subscriber.sender.send(Arc::clone(&frame)).is_ok());
    }

    #[cfg(test)]
    fn subscriber_count(&self) -> usize {
        self.subscribers.lock().expect("sse hub lock").len()
    }
}

// ---- HTTP responses -------------------------------------------------------

fn respond_json(request: Request, status: u16, value: &Value, cors: Option<&str>) -> Result<()> {
    let body =
        serde_json::to_string(value).map_err(|err| anyhow!("encode /rpc response: {err}"))?;
    let response = with_cors(
        Response::from_string(body)
            .with_status_code(StatusCode(status))
            .with_header(static_header("Content-Type", "application/json")),
        cors,
    );
    request
        .respond(response)
        .map_err(|err| anyhow!("failed to write /rpc response: {err}"))
}

fn respond_html(request: Request, html: &str, cors: Option<&str>) -> Result<()> {
    let response = with_cors(
        Response::from_string(html.to_string())
            .with_status_code(StatusCode(200))
            .with_header(static_header("Content-Type", "text/html; charset=utf-8")),
        cors,
    );
    request
        .respond(response)
        .map_err(|err| anyhow!("failed to write GUI response: {err}"))
}

fn respond_text(request: Request, status: u16, message: &str, cors: Option<&str>) -> Result<()> {
    let response = with_cors(
        Response::from_string(message.to_string())
            .with_status_code(StatusCode(status))
            .with_header(static_header("Content-Type", "text/plain; charset=utf-8")),
        cors,
    );
    request
        .respond(response)
        .map_err(|err| anyhow!("failed to write response: {err}"))
}

fn respond_preflight(request: Request, cors: Option<&str>) -> Result<()> {
    let response = with_cors(
        Response::empty(StatusCode(204))
            .with_header(static_header(
                "Access-Control-Allow-Methods",
                "GET, POST, OPTIONS",
            ))
            .with_header(static_header(
                "Access-Control-Allow-Headers",
                "Content-Type, Authorization",
            )),
        cors,
    );
    request
        .respond(response)
        .map_err(|err| anyhow!("failed to write preflight response: {err}"))
}

/// Attach the CORS echo headers iff an origin was allowed (loopback). A request
/// with no allowed origin gets no `Access-Control-Allow-Origin`, so a foreign
/// web page cannot read the response — replacing the former wildcard `*`.
fn with_cors<R: std::io::Read>(response: Response<R>, cors: Option<&str>) -> Response<R> {
    match cors.and_then(|origin| dyn_header("Access-Control-Allow-Origin", origin)) {
        Some(header) => response
            .with_header(header)
            .with_header(static_header("Vary", "Origin")),
        None => response,
    }
}

fn static_header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("static HTTP header is valid")
}

/// Build a header from a runtime-derived value, dropping it if the value is not
/// a valid header (never the case for an origin we already parsed, but keeps the
/// transport panic-free on hostile input).
fn dyn_header(name: &str, value: &str) -> Option<Header> {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).ok()
}

// ---- request inspection + transport-guard helpers -------------------------

/// Case-insensitive lookup of a request header value. `name` is `'static`
/// because `tiny_http`'s [`HeaderField::equiv`] requires it; every caller passes
/// a string literal.
fn header_value<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv(name))
        .map(|header| header.value.as_str())
}

/// The CORS origin to echo: the request `Origin` iff it is a loopback origin,
/// else `None`. Same-origin requests (which need no CORS) simply pass through.
fn allowed_origin(request: &Request) -> Option<String> {
    let origin = header_value(request, "Origin")?;
    origin_is_loopback(origin).then(|| origin.to_string())
}

fn host_is_loopback(request: &Request) -> bool {
    header_value(request, "Host").is_some_and(authority_host_is_loopback)
}

/// Whether an `Origin` (`scheme://host[:port]`) is an http(s) loopback origin.
fn origin_is_loopback(origin: &str) -> bool {
    match origin.split_once("://") {
        Some((scheme, authority)) if scheme == "http" || scheme == "https" => {
            authority_host_is_loopback(authority)
        }
        _ => false,
    }
}

/// Whether an authority (`host` or `host:port`, incl. `[ipv6]:port`) is loopback.
fn authority_host_is_loopback(authority: &str) -> bool {
    let host = strip_port(authority.trim());
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Strip a trailing `:port` from an authority, handling bracketed IPv6 literals
/// (`[::1]:4173` -> `::1`) and bare addresses (`::1`, `127.0.0.1`) unchanged.
fn strip_port(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    if authority.parse::<IpAddr>().is_ok() {
        return authority; // bare IPv4/IPv6 with no port
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => authority,
    }
}

/// Extract the token from an `Authorization: Bearer <token>` header value.
fn bearer_from_header(value: &str) -> Option<&str> {
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?
        .trim();
    (!token.is_empty()).then_some(token)
}

/// Find `key`'s value in a raw `a=1&b=2` query string. The daemon token is
/// base64url-no-pad, so it is never percent-encoded and compares raw.
fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then_some(v)
    })
}

/// Constant-time byte comparison so token checking does not leak length/prefix
/// information through timing.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Policy;
    use crate::providers::ProviderRegistry;
    use crate::tools;
    use crate::workspace::{args_with, registry};
    use serde_json::json;
    use std::fs;

    fn test_router() -> (tempfile::TempDir, RuntimeDaemonRouter) {
        let root = tempfile::tempdir().expect("tempdir");
        fs::write(root.path().join("notes.txt"), "alpha\n").expect("write notes");
        let runtime = tools::runtime(
            registry(&args_with(vec![root.path().to_path_buf()], Vec::new())).expect("registry"),
        );
        let router = RuntimeDaemonRouter::new(
            Arc::new(runtime),
            ProviderRegistry::default(),
            Policy::default(),
            None,
            |_| {},
        );
        (root, router)
    }

    #[test]
    fn route_rpc_body_returns_runtime_info() {
        let (_root, router) = test_router();
        let response = route_rpc_body(
            &router,
            r#"{"jsonrpc":"2.0","id":1,"method":"runtime/info"}"#,
        )
        .expect("runtime/info response");
        assert_eq!(response["result"]["protocol"], "nerve-runtime");
        assert_eq!(response["id"], json!(1));
    }

    #[test]
    fn route_rpc_body_lists_tools() {
        let (_root, router) = test_router();
        let response = route_rpc_body(
            &router,
            r#"{"jsonrpc":"2.0","id":2,"method":"runtime/tools/list"}"#,
        )
        .expect("tools/list response");
        assert!(
            response["result"]["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty())
        );
    }

    #[test]
    fn gui_html_is_embedded_and_targets_runtime_endpoints() {
        // The GUI is a client of Protocol v3: it must talk to `/rpc` + `/events`
        // only, and drive agent runs through the existing job methods.
        assert!(GUI_HTML.contains("<!doctype html"));
        assert!(GUI_HTML.contains("/rpc"));
        assert!(GUI_HTML.contains("/events"));
        assert!(GUI_HTML.contains("runtime/info"));
        assert!(GUI_HTML.contains("runtime/tools/list"));
        assert!(GUI_HTML.contains("runtime/jobs/start"));
        assert!(GUI_HTML.contains("runtime/jobs/get"));
        assert!(GUI_HTML.contains("runtime/jobs/cancel"));
        assert!(GUI_HTML.contains("agent.run"));
    }

    #[test]
    fn gui_html_drives_the_session_chat_surface() {
        // S3: the primary surface is a multi-turn chat over the `session.*`
        // command family, including the approval round-trip. All of this rides
        // the same Protocol v3 jobs/events — no new transport or method.
        assert!(GUI_HTML.contains("session.start"));
        assert!(GUI_HTML.contains("session.message"));
        assert!(GUI_HTML.contains("session.respond"));
        assert!(GUI_HTML.contains("session.interrupt"));
        assert!(GUI_HTML.contains("approval_requested"));
    }

    #[test]
    fn route_rpc_body_reports_parse_error_with_null_id() {
        let (_root, router) = test_router();
        let response = route_rpc_body(&router, "not json").expect("parse-error response");
        assert_eq!(response["error"]["code"], json!(-32700));
        assert_eq!(response["id"], Value::Null);
    }

    #[test]
    fn route_rpc_body_has_no_response_for_notification() {
        let (_root, router) = test_router();
        // No `id` => JSON-RPC notification => the router emits no response.
        let response = route_rpc_body(&router, r#"{"jsonrpc":"2.0","method":"runtime/info"}"#);
        assert!(response.is_none());
    }

    #[test]
    fn sse_data_frame_wraps_json_payload() {
        assert_eq!(sse_data_frame(r#"{"a":1}"#), "data: {\"a\":1}\n\n");
    }

    #[test]
    fn write_sse_chunk_emits_hex_length_framing() {
        let mut buf = Vec::new();
        write_sse_chunk(&mut buf, b"data: x\n\n").expect("write chunk");
        // 9-byte payload => hex length `9`, CRLF-delimited chunk.
        assert_eq!(
            String::from_utf8(buf).expect("utf8"),
            "9\r\ndata: x\n\n\r\n"
        );
    }

    #[test]
    fn write_sse_head_announces_event_stream() {
        let mut buf = Vec::new();
        write_sse_head(&mut buf, None).expect("write head");
        let head = String::from_utf8(buf).expect("utf8");
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(head.contains("Content-Type: text/event-stream\r\n"));
        assert!(head.contains("Transfer-Encoding: chunked\r\n"));
        assert!(head.ends_with("\r\n\r\n"));
        // No allowed origin => no CORS echo on the stream.
        assert!(!head.contains("Access-Control-Allow-Origin"));
    }

    #[test]
    fn write_sse_head_echoes_allowed_origin() {
        let mut buf = Vec::new();
        write_sse_head(&mut buf, Some("http://127.0.0.1:4173")).expect("write head");
        let head = String::from_utf8(buf).expect("utf8");
        assert!(head.contains("Access-Control-Allow-Origin: http://127.0.0.1:4173\r\n"));
        assert!(head.contains("Vary: Origin\r\n"));
    }

    #[test]
    fn token_matches_accepts_header_or_query_and_rejects_otherwise() {
        let security = HttpSecurity {
            token: "s3cret".to_string(),
            enforce_loopback_host: true,
        };
        assert!(security.token_matches(Some("Bearer s3cret"), None));
        assert!(security.token_matches(Some("bearer s3cret"), None));
        assert!(security.token_matches(None, Some("x=1&token=s3cret&y=2")));
        // A query token wins when the header is absent/non-bearer.
        assert!(security.token_matches(Some("Basic zzz"), Some("token=s3cret")));
        assert!(!security.token_matches(Some("Bearer nope"), Some("token=nope")));
        assert!(!security.token_matches(None, None));
        assert!(!security.token_matches(Some("Bearer "), None));
        // A token that is a prefix of the real one must not pass (length check).
        assert!(!security.token_matches(Some("Bearer s3cre"), None));
    }

    #[test]
    fn render_gui_embeds_token_only_on_loopback() {
        let local = HttpSecurity {
            token: "TOKEN123".to_string(),
            enforce_loopback_host: true,
        };
        let rendered = local.render_gui("auth=__NERVE_DAEMON_TOKEN__;");
        assert_eq!(rendered, "auth=TOKEN123;");

        let remote = HttpSecurity {
            token: "TOKEN123".to_string(),
            enforce_loopback_host: false,
        };
        // A remote bind never bakes the token into the served page.
        assert_eq!(
            remote.render_gui("auth=__NERVE_DAEMON_TOKEN__;"),
            "auth=__NERVE_DAEMON_TOKEN__;"
        );
    }

    #[test]
    fn loopback_authorities_and_origins_are_recognized() {
        for ok in [
            "127.0.0.1",
            "127.0.0.1:4173",
            "localhost",
            "LocalHost:8080",
            "[::1]:4173",
            "::1",
            "127.5.6.7:80",
        ] {
            assert!(authority_host_is_loopback(ok), "{ok} should be loopback");
        }
        for bad in ["0.0.0.0", "192.168.1.5:4173", "example.com", "10.0.0.1"] {
            assert!(
                !authority_host_is_loopback(bad),
                "{bad} should not be loopback"
            );
        }
        assert!(origin_is_loopback("http://127.0.0.1:4173"));
        assert!(origin_is_loopback("https://localhost"));
        assert!(!origin_is_loopback("http://evil.example.com"));
        assert!(!origin_is_loopback("null"));
        assert!(!origin_is_loopback("file://127.0.0.1"));
    }

    #[test]
    fn bearer_and_query_token_parsing() {
        assert_eq!(bearer_from_header("Bearer abc"), Some("abc"));
        assert_eq!(bearer_from_header("Token abc"), None);
        assert_eq!(query_param("token=abc&x=1", "token"), Some("abc"));
        assert_eq!(query_param("x=1", "token"), None);
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }

    #[test]
    fn hub_broadcasts_to_live_subscribers_and_prunes_dead_ones() {
        let hub = SseHub::new();
        let (_id_a, rx_a) = hub.subscribe();
        let (_id_b, rx_b) = hub.subscribe();
        assert_eq!(hub.subscriber_count(), 2);
        drop(rx_b); // a disconnected client
        hub.broadcast(json!({ "method": "runtime/event", "params": { "type": "ping" } }));
        let frame = rx_a.recv().expect("frame");
        assert!(frame.contains("runtime/event"));
        // The dead subscriber was pruned on its failed send.
        assert_eq!(hub.subscriber_count(), 1);
    }
}
