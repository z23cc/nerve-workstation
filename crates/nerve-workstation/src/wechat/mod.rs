//! The daemon-hosted WeChat (个人微信) bridge — the in-process replacement for the
//! standalone `nerve-wechat` binary's child-daemon model. It owns the logged-in
//! gateway session and the long-poll bridge thread; lifecycle is driven entirely by
//! the `wechat.*` runtime commands ([`crate::jobs::JobManager`] routes them here),
//! and every step is surfaced as a global [`RuntimeEvent::Wechat`] so any connected
//! GUI/TUI sees login + bridge status live.
//!
//! Threading: the gateway is blocking `ureq` (a ~40s long-poll), so the bridge runs
//! on its own dedicated `std::thread`; `wechat.login` runs on the calling job thread
//! (cancellable). Nothing here ever blocks the daemon's dispatch path.

mod control;

use control::RuntimeNerve;
use nerve_core::CancelToken;
use nerve_runtime::DelegateAutonomy;
use nerve_runtime::{RuntimeError, RuntimeEvent, WechatEventKind};
use nerve_wechat::{
    Bridge, DEFAULT_BASE_URL, IlinkGateway, QrStatus, SenderAllowlist, WeixinSession, http,
    poll_qr_once, start_qr_login,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::sandbox::SandboxLauncher;
use crate::sync::lock_recover;

/// The shared event sink type (matches `JobManager`'s `Arc<EventEmitter>`).
pub(crate) type Emit = Arc<dyn Fn(RuntimeEvent) + Send + Sync + 'static>;

/// QR scan window (matches the plugin's 480s login timeout).
const LOGIN_TIMEOUT: Duration = Duration::from_secs(480);
/// Poll cadence while waiting for the QR to be scanned/confirmed.
const LOGIN_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Per-request timeout for the login bootstrap calls.
const LOGIN_HTTP_TIMEOUT: Duration = Duration::from_secs(40);

/// A running bridge: its cooperative stop flag, the thread handle, and the config
/// it was started with (surfaced by `wechat.status`).
struct RunningBridge {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
    owners: Vec<String>,
    agent: String,
}

/// The daemon's WeChat host: a logged-in session plus an optional running bridge.
pub(crate) struct WechatHost {
    emit: Emit,
    session: Mutex<Option<WeixinSession>>,
    bridge: Mutex<Option<RunningBridge>>,
}

impl WechatHost {
    pub(crate) fn new(emit: Emit) -> Self {
        Self {
            emit,
            session: Mutex::new(None),
            bridge: Mutex::new(None),
        }
    }

    fn emit(&self, kind: WechatEventKind) {
        (self.emit)(RuntimeEvent::wechat(kind));
    }

    /// `wechat.login`: run the QR flow on the calling job thread (cancellable),
    /// caching the confirmed session for a later [`Self::start`]. Emits a `LoginQr`
    /// event with the scannable image, then `LoginStatus` transitions, then
    /// `LoggedIn` (or `LoginFailed`).
    pub(crate) fn login(
        &self,
        bot_type: &str,
        base_url: Option<&str>,
        token: &CancelToken,
    ) -> Result<Value, RuntimeError> {
        let bootstrap = base_url.unwrap_or(DEFAULT_BASE_URL);
        let agent = http::agent(LOGIN_HTTP_TIMEOUT);
        let start = start_qr_login(&agent, bootstrap, bot_type).map_err(login_err)?;
        self.emit(WechatEventKind::LoginQr {
            qrcode: start.qrcode.clone(),
            image_url: start.image_url.clone(),
        });
        self.poll_login(&agent, bootstrap, &start.qrcode, token)
    }

    /// Poll the QR status to a terminal state, emitting status transitions. On
    /// `Confirmed` the session is cached and a `LoggedIn` event is emitted.
    fn poll_login(
        &self,
        agent: &ureq::Agent,
        bootstrap: &str,
        qrcode: &str,
        token: &CancelToken,
    ) -> Result<Value, RuntimeError> {
        let deadline = Instant::now() + LOGIN_TIMEOUT;
        let mut last = String::new();
        loop {
            if token.is_cancelled() {
                return Err(RuntimeError::cancelled());
            }
            if Instant::now() >= deadline {
                return Err(self.login_failed("QR login timed out before scan"));
            }
            let (status, session) = poll_qr_once(agent, bootstrap, qrcode).map_err(login_err)?;
            let label = status_label(&status);
            if label != last {
                last = label.to_string();
                self.emit(WechatEventKind::LoginStatus {
                    status: label.to_string(),
                });
            }
            match status {
                QrStatus::Confirmed => return self.confirm_login(session),
                QrStatus::Expired => return Err(self.login_failed("QR code expired")),
                QrStatus::VerifyBlocked => return Err(self.login_failed("verification blocked")),
                _ => std::thread::sleep(LOGIN_POLL_INTERVAL),
            }
        }
    }

    /// Cache a confirmed session and emit `LoggedIn`.
    fn confirm_login(&self, session: Option<WeixinSession>) -> Result<Value, RuntimeError> {
        let session = session
            .ok_or_else(|| RuntimeError::adapter("wechat login: confirmed without a token"))?;
        let account_id = session.account_id.clone();
        let user_id = session.user_id.clone();
        *lock_recover(&self.session) = Some(session);
        self.emit(WechatEventKind::LoggedIn {
            account_id: account_id.clone(),
            user_id: user_id.clone(),
        });
        Ok(json!({ "logged_in": true, "account_id": account_id, "user_id": user_id }))
    }

    fn login_failed(&self, error: &str) -> RuntimeError {
        self.emit(WechatEventKind::LoginFailed {
            error: error.to_string(),
        });
        RuntimeError::adapter(format!("wechat login: {error}"))
    }

    /// `wechat.start`: build the gateway from the logged-in session and spawn the
    /// stop-checked bridge thread, driving delegated turns in-process. Errors if not
    /// logged in or already running.
    pub(crate) fn start(
        &self,
        launcher: Arc<dyn SandboxLauncher>,
        root: PathBuf,
        owners: Vec<String>,
        agent: String,
        autonomy: DelegateAutonomy,
    ) -> Result<Value, RuntimeError> {
        let session = lock_recover(&self.session).clone().ok_or_else(|| {
            RuntimeError::adapter("wechat: not logged in — run wechat.login first")
        })?;
        let mut guard = lock_recover(&self.bridge);
        if guard.as_ref().is_some_and(|b| !b.handle.is_finished()) {
            return Err(RuntimeError::adapter(
                "wechat bridge already running — stop it (wechat.stop) first",
            ));
        }
        let account_id = session.account_id.clone();
        let user_id = session.user_id.clone();
        let nerve = RuntimeNerve::new(
            launcher,
            root,
            agent.clone(),
            autonomy,
            Arc::clone(&self.emit),
        );
        let bridge = Bridge::new(
            IlinkGateway::new(session),
            nerve,
            SenderAllowlist::new(owners.clone()),
            account_id.clone(),
            user_id.clone(),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let emit = Arc::clone(&self.emit);
        let (acct, usr) = (account_id.clone(), user_id.clone());
        let handle = std::thread::Builder::new()
            .name("nerve-wechat-bridge".to_string())
            .spawn(move || run_bridge_thread(bridge, &stop_thread, &emit, acct, usr))
            .map_err(|err| RuntimeError::adapter(format!("spawn wechat bridge: {err}")))?;
        *guard = Some(RunningBridge {
            stop,
            handle,
            owners,
            agent,
        });
        drop(guard);
        self.emit(WechatEventKind::BridgeStatus {
            running: true,
            account_id,
            user_id,
        });
        Ok(json!({ "running": true }))
    }

    /// `wechat.stop`: signal the bridge thread and join it. Idempotent.
    pub(crate) fn stop(&self) -> Result<Value, RuntimeError> {
        let running = lock_recover(&self.bridge).take();
        match running {
            Some(b) => {
                b.stop.store(true, Ordering::Relaxed);
                let _ = b.handle.join();
                Ok(json!({ "stopped": true }))
            }
            None => Ok(json!({ "stopped": false })),
        }
    }

    /// `wechat.status`: report login + bridge state without secrets.
    pub(crate) fn status(&self) -> Value {
        let session = lock_recover(&self.session).clone();
        let mut guard = lock_recover(&self.bridge);
        if guard.as_ref().is_some_and(|b| b.handle.is_finished()) {
            *guard = None; // reap a bridge whose thread already exited
        }
        let (account_id, user_id) = session
            .as_ref()
            .map(|s| (s.account_id.clone(), s.user_id.clone()))
            .unwrap_or_default();
        json!({
            "logged_in": session.is_some(),
            "running": guard.is_some(),
            "account_id": account_id,
            "user_id": user_id,
            "owners": guard.as_ref().map(|b| b.owners.clone()).unwrap_or_default(),
            "agent": guard.as_ref().map(|b| b.agent.clone()),
        })
    }
}

/// The bridge thread body: run the long-poll loop until stopped, then emit a final
/// `BridgeStatus { running: false }` (and a `LoginFailed` carrying the reason on a
/// fatal error, so a client surfaces it rather than a silent stop).
fn run_bridge_thread(
    mut bridge: Bridge<IlinkGateway, RuntimeNerve>,
    stop: &Arc<AtomicBool>,
    emit: &Emit,
    account_id: String,
    user_id: String,
) {
    let should_stop = || stop.load(Ordering::Relaxed);
    if let Err(err) = bridge.run_until(&should_stop) {
        emit(RuntimeEvent::wechat(WechatEventKind::LoginFailed {
            error: format!("wechat bridge stopped: {err}"),
        }));
    }
    emit(RuntimeEvent::wechat(WechatEventKind::BridgeStatus {
        running: false,
        account_id,
        user_id,
    }));
}

/// Map a [`QrStatus`] to the wire label surfaced in a `LoginStatus` event.
fn status_label(status: &QrStatus) -> &str {
    match status {
        QrStatus::Wait => "wait",
        QrStatus::Scanned => "scanned",
        QrStatus::Confirmed => "confirmed",
        QrStatus::Expired => "expired",
        QrStatus::Redirect => "redirect",
        QrStatus::NeedVerifyCode => "need_verifycode",
        QrStatus::VerifyBlocked => "verify_blocked",
        QrStatus::Unknown(other) => other.as_str(),
    }
}

fn login_err(err: nerve_wechat::WeixinError) -> RuntimeError {
    RuntimeError::adapter(format!("wechat login: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> WechatHost {
        let emit: Emit = Arc::new(|_event: RuntimeEvent| {});
        WechatHost::new(emit)
    }

    #[test]
    fn status_before_login_reports_logged_out_and_idle() {
        let status = host().status();
        assert_eq!(status["logged_in"], false);
        assert_eq!(status["running"], false);
        assert_eq!(status["account_id"], "");
        assert_eq!(status["agent"], serde_json::Value::Null);
    }

    #[test]
    fn stop_is_idempotent_when_not_running() {
        assert_eq!(host().stop().expect("stop")["stopped"], false);
    }

    #[test]
    fn start_without_login_is_refused() {
        let err = host()
            .start(
                crate::sandbox::refuse_launcher(),
                PathBuf::from("/tmp"),
                vec!["u_owner".to_string()],
                "claude".to_string(),
                DelegateAutonomy::ReadOnly,
            )
            .expect_err("start should refuse without a session");
        assert!(err.to_string().contains("not logged in"));
    }

    #[test]
    fn status_label_maps_known_and_unknown_states() {
        assert_eq!(status_label(&QrStatus::Wait), "wait");
        assert_eq!(status_label(&QrStatus::Confirmed), "confirmed");
        assert_eq!(status_label(&QrStatus::VerifyBlocked), "verify_blocked");
        assert_eq!(status_label(&QrStatus::Unknown("weird".into())), "weird");
    }
}
