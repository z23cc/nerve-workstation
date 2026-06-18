use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use nerve_agent::auth::{self, AuthMode, Credential, LoginStart, ProviderId};
use nerve_agent::error::AgentError;
use nerve_core::CancelToken;
use nerve_runtime::{RuntimeCommand, RuntimeError};
use serde_json::{Value, json};

#[derive(Default)]
pub(crate) struct AuthManager {
    pending: Mutex<HashMap<String, PendingLogin>>,
}

#[derive(Clone)]
struct PendingLogin {
    start: LoginStart,
    created_at_ms: u64,
}

impl AuthManager {
    pub(crate) fn handle_command(
        &self,
        command: RuntimeCommand,
        cancel: &CancelToken,
    ) -> Result<Value, RuntimeError> {
        if cancel.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        match command {
            RuntimeCommand::AuthStart { provider } => self.start(&provider, cancel),
            RuntimeCommand::AuthComplete {
                login_id,
                code,
                callback_url,
            } => self.complete(&login_id, code, callback_url, cancel),
            RuntimeCommand::AuthStatus { provider } => self.status(&provider),
            RuntimeCommand::AuthLogout { provider } => self.logout(&provider),
            _ => Err(RuntimeError::adapter("expected auth.* command")),
        }
    }

    fn start(&self, provider: &str, cancel: &CancelToken) -> Result<Value, RuntimeError> {
        let provider = parse_provider(provider)?;
        let strategy = auth::strategy_for(provider);
        let redirect_uri = strategy.default_redirect_uri();
        if cancel.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        let start = strategy.start(&redirect_uri).map_err(agent_runtime_error)?;
        if cancel.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        let login_id = format!("login-{}", auth::oauth::random_urlsafe(18));
        self.pending.lock().expect("auth pending lock").insert(
            login_id.clone(),
            PendingLogin {
                start: start.clone(),
                created_at_ms: now_ms(),
            },
        );
        Ok(json!({
            "login_id": login_id,
            "provider": provider.as_str(),
            "authorize_url": start.authorize_url,
            "redirect_uri": start.redirect_uri,
        }))
    }

    fn complete(
        &self,
        login_id: &str,
        code: Option<String>,
        callback_url: Option<String>,
        cancel: &CancelToken,
    ) -> Result<Value, RuntimeError> {
        let input = callback_url.or(code).ok_or_else(|| {
            RuntimeError::adapter("auth.complete requires `code` or `callback_url`")
        })?;
        let pending = self
            .pending
            .lock()
            .expect("auth pending lock")
            .get(login_id)
            .cloned()
            .ok_or_else(|| RuntimeError::adapter(format!("unknown auth login_id: {login_id}")))?;
        let callback = auth::oauth::parse_pasted_callback(input.trim());
        let strategy = auth::strategy_for(pending.start.provider);
        let credential = strategy
            .complete(&pending.start, &callback, cancel)
            .map_err(agent_runtime_error)?;
        auth::save_credential(&credential).map_err(agent_runtime_error)?;
        self.pending
            .lock()
            .expect("auth pending lock")
            .remove(login_id);
        Ok(json!({
            "login_id": login_id,
            "created_at_ms": pending.created_at_ms,
            "credential": credential_status(&credential),
        }))
    }

    fn status(&self, provider: &str) -> Result<Value, RuntimeError> {
        let provider = parse_provider(provider)?;
        match auth::load_credential(provider).map_err(agent_runtime_error)? {
            Some(credential) => Ok(credential_status(&credential)),
            None => Ok(json!({
                "provider": provider.as_str(),
                "status": "not_logged_in",
            })),
        }
    }

    fn logout(&self, provider: &str) -> Result<Value, RuntimeError> {
        let provider = parse_provider(provider)?;
        auth::delete_credential(provider).map_err(agent_runtime_error)?;
        self.pending
            .lock()
            .expect("auth pending lock")
            .retain(|_, pending| pending.start.provider != provider);
        Ok(json!({
            "provider": provider.as_str(),
            "status": "logged_out",
        }))
    }
}

fn parse_provider(provider: &str) -> Result<ProviderId, RuntimeError> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "anthropic" | "claude" => Ok(ProviderId::Anthropic),
        "openai" | "chatgpt" | "openai_responses" => Ok(ProviderId::OpenAi),
        "xai" | "grok" => Ok(ProviderId::Xai),
        _ => Err(RuntimeError::adapter(format!(
            "unknown auth provider '{provider}': expected anthropic|openai|xai"
        ))),
    }
}

fn credential_status(credential: &Credential) -> Value {
    json!({
        "provider": credential.provider.as_str(),
        "status": "authenticated",
        "mode": mode_label(credential.mode),
        "base_url": credential.base_url,
        "account_id": credential.account_id,
        "expires_at_unix": credential.expires_at_unix,
    })
}

fn mode_label(mode: AuthMode) -> &'static str {
    match mode {
        AuthMode::ApiKey => "api_key",
        AuthMode::Oauth => "oauth",
    }
}

fn agent_runtime_error(error: AgentError) -> RuntimeError {
    if matches!(error, AgentError::Cancelled) {
        RuntimeError::cancelled()
    } else {
        RuntimeError::adapter(error.to_string())
    }
}

fn now_ms() -> u64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_provider_accepts_aliases() {
        assert_eq!(
            parse_provider("claude").expect("claude"),
            ProviderId::Anthropic
        );
        assert_eq!(
            parse_provider("chatgpt").expect("chatgpt"),
            ProviderId::OpenAi
        );
        assert_eq!(parse_provider("grok").expect("grok"), ProviderId::Xai);
        assert!(parse_provider("unknown").is_err());
    }

    #[test]
    fn credential_status_does_not_include_secrets() {
        let credential = Credential {
            provider: ProviderId::OpenAi,
            mode: AuthMode::Oauth,
            access_token: "access-secret".into(),
            refresh_token: Some("refresh-secret".into()),
            expires_at_unix: Some(123),
            account_id: Some("acct".into()),
            base_url: ProviderId::OpenAi.default_base_url().to_string(),
        };
        let status = credential_status(&credential);
        assert_eq!(status["provider"], "openai");
        assert_eq!(status["mode"], "oauth");
        assert!(status.get("access_token").is_none());
        assert!(status.get("refresh_token").is_none());
    }
}
