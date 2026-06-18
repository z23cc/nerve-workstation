use super::*;

#[test]
fn from_api_key_uses_default_base_url_when_absent() {
    let cred = from_api_key(ProviderId::Xai, "  sk-xai  ", None);
    assert_eq!(cred.mode, AuthMode::ApiKey);
    assert_eq!(cred.access_token, "sk-xai");
    assert_eq!(cred.base_url, "https://api.x.ai");
    assert!(cred.refresh_token.is_none());
    assert!(cred.expires_at_unix.is_none());
}

#[test]
fn from_api_key_trims_trailing_slash_on_custom_base_url() {
    let cred = from_api_key(ProviderId::OpenAi, "k", Some("https://proxy.example/v1/"));
    assert_eq!(cred.base_url, "https://proxy.example/v1");
}

#[test]
fn strategy_for_returns_matching_provider() {
    assert_eq!(
        strategy_for(ProviderId::Anthropic).provider(),
        ProviderId::Anthropic
    );
    assert_eq!(
        strategy_for(ProviderId::OpenAi).provider(),
        ProviderId::OpenAi
    );
    assert_eq!(strategy_for(ProviderId::Xai).provider(), ProviderId::Xai);
}

#[test]
fn staged_start_contains_authorize_url_and_context() {
    let strategy = strategy_for(ProviderId::OpenAi);
    let start = strategy
        .start("http://localhost:1455/auth/callback")
        .expect("start");
    assert_eq!(start.provider, ProviderId::OpenAi);
    assert_eq!(start.redirect_uri, "http://localhost:1455/auth/callback");
    assert!(!start.state.is_empty());
    assert!(!start.verifier.is_empty());
    assert!(start.authorize_url.contains("code_challenge="));
    assert!(start.authorize_url.contains("state="));
}

#[test]
fn default_redirect_uri_uses_provider_loopback() {
    assert_eq!(
        strategy_for(ProviderId::Anthropic).default_redirect_uri(),
        "http://localhost:54545/callback"
    );
    assert_eq!(
        strategy_for(ProviderId::OpenAi).default_redirect_uri(),
        "http://localhost:1455/auth/callback"
    );
    assert_eq!(
        strategy_for(ProviderId::Xai).default_redirect_uri(),
        "http://127.0.0.1:56121/callback"
    );
}

#[test]
fn anthropic_authorize_url_has_pkce_and_scopes() {
    let pkce = Pkce::generate();
    let url = AnthropicAuth::authorize_url("http://localhost:54545/callback", "st4te", &pkce)
        .expect("url");
    assert!(url.starts_with("https://claude.ai/oauth/authorize?"));
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains("state=st4te"));
    assert!(url.contains("code=true"));
    assert!(url.contains(&urlencoding_fragment(&pkce.challenge)));
}

#[test]
fn openai_authorize_url_has_codex_flags() {
    let pkce = Pkce::generate();
    let url =
        OpenAiAuth::authorize_url("http://localhost:1455/auth/callback", "s", &pkce).expect("url");
    assert!(url.contains("codex_cli_simplified_flow=true"));
    assert!(url.contains("id_token_add_organizations=true"));
    assert!(url.contains("originator=nerve"));
}

#[test]
fn openai_token_profile_reads_account_and_email() {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
    let claims = serde_json::json!({
        (OPENAI_JWT_AUTH_CLAIM): { "chatgpt_account_id": "acc_42" },
        (OPENAI_JWT_PROFILE_CLAIM): { "email": "User@Example.COM" },
    });
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let token = format!("{header}.{payload}.sig");
    let (account, email) = OpenAiAuth::token_profile(&token);
    assert_eq!(account.as_deref(), Some("acc_42"));
    assert_eq!(email.as_deref(), Some("user@example.com"));
}

#[test]
fn openai_credential_uses_expiry_skew() {
    let now = oauth::now_unix();
    let value = serde_json::json!({
        "access_token": "not-a-jwt",
        "refresh_token": "rt",
        "expires_in": 3600,
    });
    let cred = OpenAiAuth::credential_from_token(&value, false).expect("cred");
    let expires = cred.expires_at_unix.expect("expiry");
    assert!(expires >= now + 3600 - EXPIRY_SKEW_SECS - 2);
    assert!(expires <= now + 3600 - EXPIRY_SKEW_SECS + 2);
}

#[test]
fn anthropic_credential_extracts_account_uuid() {
    let value = serde_json::json!({
        "access_token": "at",
        "refresh_token": "rt",
        "expires_in": 3600,
        "account": { "uuid": "u-1", "email_address": "a@b.c" },
    });
    let cred = AnthropicAuth::credential_from_token(&value).expect("cred");
    assert_eq!(cred.account_id.as_deref(), Some("u-1"));
    assert_eq!(cred.refresh_token.as_deref(), Some("rt"));
    assert!(cred.expires_at_unix.is_some());
}

#[test]
fn validate_xai_endpoint_pins_origin() {
    assert!(validate_xai_endpoint("https://auth.x.ai/token", "token_endpoint").is_ok());
    assert!(validate_xai_endpoint("https://accounts.x.ai/oauth", "authorization_endpoint").is_ok());
    assert!(validate_xai_endpoint("https://example.com/token", "token_endpoint").is_err());
    assert!(validate_xai_endpoint("http://auth.x.ai/token", "token_endpoint").is_err());
    assert!(validate_xai_endpoint("https://auth.x.ai/token?x=1", "token_endpoint").is_err());
    assert!(validate_xai_endpoint("https://user:pw@auth.x.ai/token", "token_endpoint").is_err());
}

#[test]
fn anthropic_exchange_uses_flow_state_without_fragment() {
    // Regression: the loopback callback's code carries no `#state`, so the real
    // flow state must be sent — an empty state is rejected with HTTP 400.
    let (code, state) = anthropic_code_state("auth-code-abc".to_string(), "REALSTATE");
    assert_eq!(code, "auth-code-abc");
    assert_eq!(state, "REALSTATE");
}

#[test]
fn anthropic_exchange_prefers_nonempty_code_fragment() {
    let (code, state) = anthropic_code_state("auth-code-abc#FRAG".to_string(), "REALSTATE");
    assert_eq!(code, "auth-code-abc");
    assert_eq!(state, "FRAG");
}

/// Percent-encode just the characters `url::Url` escapes in a query value,
/// so the assertion above can locate the challenge regardless of encoding.
fn urlencoding_fragment(raw: &str) -> String {
    let mut url = url::Url::parse("https://x/").unwrap();
    url.query_pairs_mut().append_pair("c", raw);
    url.query().unwrap().trim_start_matches("c=").to_string()
}
