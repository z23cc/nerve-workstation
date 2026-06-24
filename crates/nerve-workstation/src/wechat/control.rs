//! The in-process [`NerveControl`] that drives the daemon's **own** delegate
//! machinery — replacing the standalone `nerve-wechat` binary's child-daemon
//! `DelegateNerve` (which spawned `nerve daemon --stdio`). It lives here (not in
//! `nerve-wechat`) because it needs the workstation's delegate launcher, and
//! `nerve-wechat` may not depend on `nerve-workstation` (that would cycle).
//!
//! Each WeChat turn runs as a **one-shot** delegate against the in-process launcher
//! (no live-session parking). Conversation continuity across messages is not kept
//! in this slice — every allowed owner message starts a fresh delegate run; the
//! reply is the agent's final assistant text. Inbound/outbound text is mirrored as
//! [`WechatEventKind::Message`] events so any client can render a live activity log.

use crate::delegate_codex_mcp::delegate_disable_flags;
use crate::delegate_runtime::{self, DelegateAgent, DelegateOutcome, DelegateParser};
use crate::sandbox::SandboxLauncher;
use nerve_core::CancelToken;
use nerve_runtime::RuntimeError;
use nerve_wechat::{BridgeError, NerveControl, NerveReply};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::Emit;
use nerve_runtime::{DelegateAutonomy, RuntimeEvent, WechatEventKind};

/// A [`NerveControl`] that runs each WeChat turn as a one-shot delegate against the
/// daemon's in-process launcher.
pub(crate) struct RuntimeNerve {
    launcher: Arc<dyn SandboxLauncher>,
    root: PathBuf,
    agent: String,
    autonomy: DelegateAutonomy,
    emit: Emit,
}

impl RuntimeNerve {
    pub(crate) fn new(
        launcher: Arc<dyn SandboxLauncher>,
        root: PathBuf,
        agent: String,
        autonomy: DelegateAutonomy,
        emit: Emit,
    ) -> Self {
        Self {
            launcher,
            root,
            agent,
            autonomy,
            emit,
        }
    }

    fn relay(&self, chat_key: &str, from_user_id: &str, direction: &str, text: &str) {
        (self.emit)(RuntimeEvent::wechat(WechatEventKind::Message {
            chat_key: chat_key.to_string(),
            from_user_id: from_user_id.to_string(),
            direction: direction.to_string(),
            text: text.to_string(),
        }));
    }
}

impl NerveControl for RuntimeNerve {
    fn handle(
        &self,
        chat_key: &str,
        from_user_id: &str,
        _existing: Option<&str>,
        text: &str,
    ) -> Result<NerveReply, BridgeError> {
        // Each turn is a fresh one-shot delegate, so the `existing` session id is
        // unused here; relay the real per-chat key + sender the bridge passes in so
        // the activity log carries true identity.
        self.relay(chat_key, from_user_id, "in", text);
        let resolved = DelegateAgent::from_name(&self.agent)
            .map_err(|err| BridgeError::Nerve(err.to_string()))?;
        let outcome = run_delegate_oneshot(
            self.launcher.as_ref(),
            resolved,
            &self.agent,
            text,
            &self.root,
            self.autonomy,
        )
        .map_err(|err| BridgeError::Nerve(err.to_string()))?;
        let reply = if outcome.result.trim().is_empty() {
            format!(
                "(delegate `{}` finished with no text; ok={})",
                self.agent, outcome.ok
            )
        } else {
            outcome.result.clone()
        };
        self.relay(chat_key, from_user_id, "out", &reply);
        // Echo the chat_key as the session id so the bridge's SessionMap keeps a
        // stable, real per-chat key (the one-shot path has no live session to resume).
        Ok(NerveReply {
            session_id: chat_key.to_string(),
            text: reply,
        })
    }
}

/// Run a single delegate turn to completion and return its outcome. Mirrors the
/// one-shot path of `JobManager::run_delegate`, but as a free function bound only to
/// the launcher, so the WeChat bridge thread can drive it without minting a job.
pub(crate) fn run_delegate_oneshot(
    launcher: &dyn SandboxLauncher,
    resolved: DelegateAgent,
    agent_name: &str,
    task: &str,
    cwd: &Path,
    autonomy: DelegateAutonomy,
) -> Result<DelegateOutcome, RuntimeError> {
    let mcp_disable_flags = delegate_disable_flags(resolved, None);
    let invocation =
        delegate_runtime::build_command(resolved, task, cwd, autonomy, None, &mcp_disable_flags);
    let policy = delegate_runtime::delegate_policy(cwd);
    let mut parser = DelegateParser::new(resolved);
    let mut on_line = |line: &str| {
        let _ = parser.ingest(line);
    };
    let token = CancelToken::never();
    let output = launcher
        .launch_streaming(
            &invocation.spec,
            &policy,
            &invocation.stdin,
            &token,
            &mut on_line,
        )
        .map_err(|err| RuntimeError::adapter(format!("delegate `{agent_name}` failed: {err}")))?;
    Ok(parser.finish(agent_name, output.exit_code, output.timed_out))
}
