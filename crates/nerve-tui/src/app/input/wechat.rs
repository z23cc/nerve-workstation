//! The `/wechat` command handler — login, start, stop, status.
//!
//! Mirrors the `delegate.rs` split: the `Shell` impl + pure command builders.
//! All four builders are pure and unit-testable with no live client.

use nerve_runtime::{DelegateAutonomy, RuntimeCommand};

use super::super::Shell;
use super::super::state::Tone;

/// Default iLink `bot_type` for `/wechat login` — the published
/// `DEFAULT_ILINK_BOT_TYPE` (`3`), matching the daemon's own default so login is
/// scan-only and the arg can be omitted.
const DEFAULT_WECHAT_BOT_TYPE: &str = "3";

impl Shell {
    /// `/wechat login <bot_type> [base_url]`
    /// Starts a `wechat.login` job that streams QR + status events back.
    pub(super) async fn cmd_wechat(&mut self, rest: &str) {
        let mut parts = rest.splitn(2, char::is_whitespace);
        let sub = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        let args = parts.next().unwrap_or("").trim().to_string();

        match sub.as_str() {
            "login" => self.cmd_wechat_login(&args).await,
            "start" => self.cmd_wechat_start(&args).await,
            "stop" => self.cmd_wechat_stop().await,
            "status" => self.cmd_wechat_status().await,
            "" => {
                self.state.hint = "usage: /wechat login|start|stop|status".to_string();
            }
            other => {
                self.state.hint =
                    format!("unknown wechat sub-command: {other} — try login|start|stop|status");
            }
        }
    }

    /// `/wechat login [bot_type] [base_url]`
    ///
    /// Scan-only: `bot_type` is optional and defaults to `"3"`
    /// (`DEFAULT_ILINK_BOT_TYPE`, matching the daemon default), so plain
    /// `/wechat login` just fetches a scannable QR.
    async fn cmd_wechat_login(&mut self, args: &str) {
        let mut parts = args.splitn(2, char::is_whitespace);
        let bot_type = parts.next().unwrap_or("").trim();
        let bot_type = if bot_type.is_empty() {
            DEFAULT_WECHAT_BOT_TYPE.to_string()
        } else {
            bot_type.to_string()
        };
        let base_url = parts
            .next()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let command = wechat_login_command(bot_type, base_url);
        self.state
            .note("starting wechat login — scan the QR when it appears");
        if let Err(err) = self.client.start_job(command, None).await {
            self.state.push_notice(Tone::Error, err.to_string());
        }
    }

    /// `/wechat start [agent] [autonomy] [owner1,owner2,...]`
    ///
    /// Defaults: agent=`claude`, autonomy=`read_only`, owners=empty.
    /// Note: empty owners means the daemon denies all WeChat senders; configure
    /// allowed owners daemon-side or pass them here as a comma-separated list.
    async fn cmd_wechat_start(&mut self, args: &str) {
        let mut parts = args.split_whitespace();
        let agent = parts.next().unwrap_or("claude").to_string();
        let autonomy_str = parts.next().unwrap_or("read_only");
        let owners_str = parts.next().unwrap_or("").to_string();

        // Default to ReadOnly; if the token doesn't parse as autonomy, treat it
        // as an owner list below.
        let autonomy = parse_autonomy(autonomy_str).unwrap_or(DelegateAutonomy::ReadOnly);
        // If autonomy parsing failed, treat the second token as owners instead.
        let owners: Vec<String> = if parse_autonomy(autonomy_str).is_none() {
            parse_owners(autonomy_str)
        } else {
            parse_owners(&owners_str)
        };

        let command = wechat_start_command(owners, agent, autonomy);
        self.state
            .note("starting wechat bridge — requires --allow-delegate and a prior wechat.login");
        if let Err(err) = self.client.start_job(command, None).await {
            self.state.push_notice(Tone::Error, err.to_string());
        }
    }

    /// `/wechat stop`
    async fn cmd_wechat_stop(&mut self) {
        let command = wechat_stop_command();
        self.state.note("stopping wechat bridge…");
        if let Err(err) = self.client.start_job(command, None).await {
            self.state.push_notice(Tone::Error, err.to_string());
        }
    }

    /// `/wechat status`
    async fn cmd_wechat_status(&mut self) {
        let command = wechat_status_command();
        if let Err(err) = self.client.start_job(command, None).await {
            self.state.push_notice(Tone::Error, err.to_string());
        }
    }
}

// ---------------------------------------------------------------------------
// Pure command builders (unit-testable, no live client)
// ---------------------------------------------------------------------------

/// Build a `wechat.login` command. Pure; testable without a live client.
#[must_use]
pub fn wechat_login_command(bot_type: String, base_url: Option<String>) -> RuntimeCommand {
    RuntimeCommand::WechatLogin { bot_type, base_url }
}

/// Build a `wechat.start` command. Pure; testable without a live client.
/// `autonomy` defaults to `ReadOnly` when called via the TUI.
#[must_use]
pub fn wechat_start_command(
    owners: Vec<String>,
    agent: String,
    autonomy: DelegateAutonomy,
) -> RuntimeCommand {
    RuntimeCommand::WechatStart {
        owners,
        agent,
        autonomy,
    }
}

/// Build a `wechat.stop` command. Pure; testable without a live client.
#[must_use]
pub fn wechat_stop_command() -> RuntimeCommand {
    RuntimeCommand::WechatStop
}

/// Build a `wechat.status` command. Pure; testable without a live client.
#[must_use]
pub fn wechat_status_command() -> RuntimeCommand {
    RuntimeCommand::WechatStatus
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Parse `"read_only"`, `"edit"`, `"full"` (and dash variants) into
/// [`DelegateAutonomy`]. Returns `None` for unrecognized strings so the caller
/// can fall back gracefully.
fn parse_autonomy(s: &str) -> Option<DelegateAutonomy> {
    match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "read_only" | "readonly" | "ro" => Some(DelegateAutonomy::ReadOnly),
        "edit" => Some(DelegateAutonomy::Edit),
        "full" => Some(DelegateAutonomy::Full),
        _ => None,
    }
}

/// Split a comma-separated owner list (e.g. `"alice,bob"`) into a `Vec<String>`,
/// filtering blank entries.
fn parse_owners(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nerve_runtime::RuntimeCommand;

    #[test]
    fn wechat_login_command_sets_bot_type_and_optional_url() {
        let cmd = wechat_login_command("gewechat".into(), None);
        match cmd {
            RuntimeCommand::WechatLogin { bot_type, base_url } => {
                assert_eq!(bot_type, "gewechat");
                assert_eq!(base_url, None);
            }
            other => panic!("expected WechatLogin, got {other:?}"),
        }

        let cmd2 = wechat_login_command("gewechat".into(), Some("http://localhost".into()));
        match cmd2 {
            RuntimeCommand::WechatLogin { bot_type, base_url } => {
                assert_eq!(bot_type, "gewechat");
                assert_eq!(base_url.as_deref(), Some("http://localhost"));
            }
            other => panic!("expected WechatLogin, got {other:?}"),
        }
    }

    #[test]
    fn wechat_start_command_defaults_to_read_only() {
        let cmd = wechat_start_command(vec![], "claude".into(), DelegateAutonomy::ReadOnly);
        match cmd {
            RuntimeCommand::WechatStart {
                owners,
                agent,
                autonomy,
            } => {
                assert!(owners.is_empty());
                assert_eq!(agent, "claude");
                assert_eq!(autonomy, DelegateAutonomy::ReadOnly);
            }
            other => panic!("expected WechatStart, got {other:?}"),
        }
    }

    #[test]
    fn wechat_start_command_carries_owners_agent_autonomy() {
        let cmd = wechat_start_command(
            vec!["alice".into(), "bob".into()],
            "codex".into(),
            DelegateAutonomy::Edit,
        );
        match cmd {
            RuntimeCommand::WechatStart {
                owners,
                agent,
                autonomy,
            } => {
                assert_eq!(owners, vec!["alice", "bob"]);
                assert_eq!(agent, "codex");
                assert_eq!(autonomy, DelegateAutonomy::Edit);
            }
            other => panic!("expected WechatStart, got {other:?}"),
        }
    }

    #[test]
    fn wechat_stop_command_is_unit_variant() {
        assert!(matches!(wechat_stop_command(), RuntimeCommand::WechatStop));
    }

    #[test]
    fn wechat_status_command_is_unit_variant() {
        assert!(matches!(
            wechat_status_command(),
            RuntimeCommand::WechatStatus
        ));
    }

    #[test]
    fn parse_autonomy_accepts_spellings() {
        assert_eq!(
            parse_autonomy("read_only"),
            Some(DelegateAutonomy::ReadOnly)
        );
        assert_eq!(
            parse_autonomy("read-only"),
            Some(DelegateAutonomy::ReadOnly)
        );
        assert_eq!(parse_autonomy("ro"), Some(DelegateAutonomy::ReadOnly));
        assert_eq!(parse_autonomy("edit"), Some(DelegateAutonomy::Edit));
        assert_eq!(parse_autonomy("full"), Some(DelegateAutonomy::Full));
        assert_eq!(parse_autonomy("bogus"), None);
    }

    #[test]
    fn parse_owners_splits_comma_list() {
        let owners = parse_owners("alice,bob, carol");
        assert_eq!(owners, vec!["alice", "bob", "carol"]);
        assert!(parse_owners("").is_empty());
    }
}
