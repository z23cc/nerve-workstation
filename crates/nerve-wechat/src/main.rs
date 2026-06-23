//! `nerve-wechat` — bridge a personal WeChat account to a local `nerve daemon`.
//!
//! Config is environment-driven (see [`nerve_wechat::config`]). On start it obtains
//! a WeChat session (a saved token, else QR login), spawns the daemon, and runs the
//! long-poll bridge: each allowed inbound message drives a `delegate.*` turn and the
//! reply is sent back to the chat. Account safety is enforced by the fail-closed
//! sender allowlist.

use nerve_wechat::{
    Bridge, DelegateNerve, IlinkGateway, SenderAllowlist, WechatConfig, WeixinSession, http,
    qr_login,
};
use std::process::ExitCode;
use std::time::Duration;

/// QR scan window (matches the plugin's 480s login timeout).
const LOGIN_TIMEOUT: Duration = Duration::from_secs(480);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("nerve-wechat: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let cfg = WechatConfig::from_env()?;
    if cfg.owners.is_empty() {
        eprintln!(
            "warning: NERVE_WECHAT_OWNERS is empty — fail-closed, so no WeChat user can \
             drive the agent. Set it to your WeChat user id(s)."
        );
    }
    let session = obtain_session(&cfg)?;
    eprintln!(
        "nerve-wechat: logged in (account={}, user={})",
        session.account_id, session.user_id
    );

    let account_id = session.account_id.clone();
    let bot_user_id = session.user_id.clone();
    let gateway = IlinkGateway::new(session);
    let nerve = DelegateNerve::spawn(&cfg.nerve_bin, &cfg.root, &cfg.agent, cfg.autonomy)
        .map_err(|err| err.to_string())?;
    let mut bridge = Bridge::new(
        gateway,
        nerve,
        SenderAllowlist::new(cfg.owners.clone()),
        account_id,
        bot_user_id,
    );
    eprintln!(
        "nerve-wechat: running (agent={}, root={}) — Ctrl-C to stop",
        cfg.agent,
        cfg.root.display()
    );
    bridge.run().map_err(|err| err.to_string())
}

/// Use a saved session if configured, otherwise run QR login (printing the QR URL).
fn obtain_session(cfg: &WechatConfig) -> Result<WeixinSession, String> {
    if let Some(session) = &cfg.preset_session {
        return Ok(session.clone());
    }
    let agent = http::agent(Duration::from_secs(40));
    qr_login(
        &agent,
        &cfg.bootstrap_base_url,
        &cfg.bot_type,
        LOGIN_TIMEOUT,
        |qr| {
            eprintln!(
                "nerve-wechat: scan this WeChat QR to log in:\n  {}\n  (qrcode id: {})",
                qr.image_url, qr.qrcode
            );
        },
    )
    .map_err(|err| err.to_string())
}
