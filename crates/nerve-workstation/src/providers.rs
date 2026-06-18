//! Provider registry — resolve a provider *name* (built-in or config-defined)
//! to a concrete [`LlmProvider`] at the composition root.
//!
//! Architecture north star P2 (`docs/designs/architecture-north-star.md` §6.2,
//! §7.2): model providers are addable by **config**, with no new code. A
//! `--provider-config` JSON file lists entries
//! `{ name, wire, base_url, api_key_env }`; each resolves to one of the existing
//! wire backends ([`AnthropicProvider`], [`OpenAiResponsesProvider`],
//! [`XaiProvider`]) driven by a [`Credential`] whose `base_url` and API key come
//! from the config entry plus the named environment variable.
//!
//! The seam stays [`nerve_agent::provider::LlmProvider`]; resolution happens
//! here in the binary (the sole composition root), never inside the
//! orchestrator. This module adds no behaviour to `nerve-agent` — it only wires
//! existing, `base_url`-driven impls to config data.

use anyhow::{Context, Result, anyhow, bail};
use nerve_agent::auth::{self, Credential};
use nerve_agent::provider::anthropic::AnthropicProvider;
use nerve_agent::provider::openai_responses::OpenAiResponsesProvider;
use nerve_agent::provider::xai::XaiProvider;
use nerve_agent::{LlmProvider, ProviderId};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

use crate::workspace::ServeArgs;

/// Wire format a config-defined provider speaks. Selects which existing
/// [`LlmProvider`] impl backs it; all three are `base_url`-driven, so a custom
/// endpoint works without new code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ProviderWire {
    /// Anthropic Messages API — `POST {base_url}/v1/messages`.
    Anthropic,
    /// OpenAI Chat Completions API — `POST {base_url}/v1/chat/completions`.
    OpenaiChat,
    /// OpenAI Responses API — `POST {base_url}/v1/responses`.
    OpenaiResponses,
}

impl ProviderWire {
    /// The built-in [`ProviderId`] whose request-shaping this wire reuses. For a
    /// config provider this only tags the [`Credential`]; the live endpoint is
    /// the config `base_url`.
    fn provider_id(self) -> ProviderId {
        match self {
            ProviderWire::Anthropic => ProviderId::Anthropic,
            ProviderWire::OpenaiChat => ProviderId::Xai,
            ProviderWire::OpenaiResponses => ProviderId::OpenAi,
        }
    }

    /// Build the backing impl from a resolved credential.
    fn build(self, credential: Credential) -> Box<dyn LlmProvider> {
        match self {
            ProviderWire::Anthropic => Box::new(AnthropicProvider::new(credential)),
            ProviderWire::OpenaiChat => Box::new(XaiProvider::new(credential)),
            ProviderWire::OpenaiResponses => Box::new(OpenAiResponsesProvider::new(credential)),
        }
    }
}

/// One config-defined provider entry.
#[derive(Debug, Clone, Deserialize)]
struct ProviderConfigEntry {
    /// Unique name, selected via `--provider <name>`. Must not collide with a
    /// built-in alias.
    name: String,
    /// Wire format / backing impl.
    wire: ProviderWire,
    /// API base URL (e.g. `https://gateway.internal`). Optional; falls back to
    /// the wire's default base URL when omitted or empty.
    #[serde(default)]
    base_url: Option<String>,
    /// Environment variable holding the API key (bearer token).
    api_key_env: String,
}

/// On-disk shape of the `--provider-config` file.
#[derive(Debug, Deserialize)]
struct ProviderConfigFile {
    #[serde(default)]
    providers: Vec<ProviderConfigEntry>,
}

/// Resolves a provider name to a concrete [`LlmProvider`]. Built-in names
/// (`anthropic`/`claude`, `openai`/`chatgpt`, `xai`/`grok`) are always
/// available; config entries extend the set without code. Built-ins take
/// precedence — a config entry may not shadow a built-in alias.
#[derive(Debug, Default, Clone)]
pub(crate) struct ProviderRegistry {
    configs: BTreeMap<String, ProviderConfigEntry>,
}

impl ProviderRegistry {
    /// Build from `--provider-config`, if set. Empty (built-ins only) otherwise.
    pub(crate) fn from_args(args: &ServeArgs) -> Result<Self> {
        match args.provider_config.as_deref() {
            Some(path) => Self::load(path),
            None => Ok(Self::default()),
        }
    }

    fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read provider config: {}", path.display()))?;
        let parsed: ProviderConfigFile = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse provider config: {}", path.display()))?;
        Self::from_entries(parsed.providers)
    }

    /// Validate and index entries by name. Rejects empty names, empty
    /// `api_key_env`, built-in shadowing, and duplicates — fail-closed so a bad
    /// config never silently resolves to the wrong provider.
    fn from_entries(entries: Vec<ProviderConfigEntry>) -> Result<Self> {
        let mut configs = BTreeMap::new();
        for entry in entries {
            let name = entry.name.trim().to_string();
            if name.is_empty() {
                bail!("provider config entry has an empty name");
            }
            if parse_builtin(&name).is_some() {
                bail!("provider config name '{name}' shadows a built-in provider");
            }
            if entry.api_key_env.trim().is_empty() {
                bail!("provider '{name}' has an empty api_key_env");
            }
            if configs.contains_key(&name) {
                bail!("duplicate provider config name '{name}'");
            }
            configs.insert(name, entry);
        }
        Ok(Self { configs })
    }

    /// Resolve `name` (built-in or config) to a provider. `api_key` overrides any
    /// stored login / environment variable.
    pub(crate) fn resolve(
        &self,
        name: &str,
        api_key: Option<&str>,
    ) -> Result<Box<dyn LlmProvider>> {
        if let Some(builtin) = parse_builtin(name) {
            let credential = builtin_credential(builtin, api_key)?;
            return Ok(build_builtin(builtin, credential));
        }
        let entry = self.configs.get(name).ok_or_else(|| {
            anyhow!(
                "unknown provider '{name}': not a built-in (anthropic|openai|xai) \
                 and not defined in --provider-config"
            )
        })?;
        let token = config_token(entry, api_key)?;
        let credential =
            auth::from_api_key(entry.wire.provider_id(), &token, entry.base_url.as_deref());
        Ok(entry.wire.build(credential))
    }
}

/// Map a provider name (and its aliases) to a built-in [`ProviderId`], or `None`
/// if it is not a built-in. Shared by resolution and config validation so the
/// reserved-name set stays in one place.
fn parse_builtin(name: &str) -> Option<ProviderId> {
    match name.to_ascii_lowercase().as_str() {
        "anthropic" | "claude" => Some(ProviderId::Anthropic),
        "openai" | "chatgpt" | "openai_responses" => Some(ProviderId::OpenAi),
        "xai" | "grok" => Some(ProviderId::Xai),
        _ => None,
    }
}

fn build_builtin(provider: ProviderId, credential: Credential) -> Box<dyn LlmProvider> {
    match provider {
        ProviderId::Anthropic => Box::new(AnthropicProvider::new(credential)),
        ProviderId::OpenAi => Box::new(OpenAiResponsesProvider::new(credential)),
        ProviderId::Xai => Box::new(XaiProvider::new(credential)),
    }
}

/// Resolve a built-in credential: explicit `--api-key`, else a stored login,
/// else the provider's `*_API_KEY` environment variable.
fn builtin_credential(provider: ProviderId, api_key: Option<&str>) -> Result<Credential> {
    if let Some(key) = api_key {
        return Ok(auth::from_api_key(provider, key, None));
    }
    if let Some(credential) = auth::load_credential(provider)
        .map_err(|err| anyhow!("failed to load credential: {err}"))?
    {
        return auth::ensure_fresh(credential, false)
            .map_err(|err| anyhow!("failed to refresh credential: {err}"));
    }
    if let Some(key) = builtin_env_key(provider) {
        return Ok(auth::from_api_key(provider, &key, None));
    }
    bail!(
        "no credential for {p}: run `nerve agent login --provider {choice}` or set {env}",
        p = provider.as_str(),
        choice = builtin_choice_name(provider),
        env = builtin_env_var(provider),
    )
}

/// Read a config provider's API key: explicit override, else the named env var.
fn config_token(entry: &ProviderConfigEntry, api_key: Option<&str>) -> Result<String> {
    if let Some(key) = api_key {
        return Ok(key.to_string());
    }
    let value = std::env::var(&entry.api_key_env).map_err(|_| {
        anyhow!(
            "provider '{}' needs environment variable {} (api_key_env) to be set",
            entry.name,
            entry.api_key_env
        )
    })?;
    if value.trim().is_empty() {
        bail!(
            "environment variable {} for provider '{}' is empty",
            entry.api_key_env,
            entry.name
        );
    }
    Ok(value)
}

fn builtin_env_key(provider: ProviderId) -> Option<String> {
    std::env::var(builtin_env_var(provider))
        .ok()
        .filter(|value| !value.is_empty())
}

fn builtin_env_var(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Anthropic => "ANTHROPIC_API_KEY",
        ProviderId::OpenAi => "OPENAI_API_KEY",
        ProviderId::Xai => "XAI_API_KEY",
    }
}

fn builtin_choice_name(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Anthropic => "claude",
        ProviderId::OpenAi => "chatgpt",
        ProviderId::Xai => "xai",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(json: &str) -> ProviderConfigEntry {
        serde_json::from_str(json).expect("entry parse")
    }

    #[test]
    fn parses_provider_config_file() {
        let file: ProviderConfigFile = serde_json::from_str(
            r#"{ "providers": [
                { "name": "groq", "wire": "openai-chat",
                  "base_url": "https://api.groq.com/openai", "api_key_env": "GROQ_API_KEY" }
            ] }"#,
        )
        .expect("parse");
        assert_eq!(file.providers.len(), 1);
        let parsed = &file.providers[0];
        assert_eq!(parsed.name, "groq");
        assert_eq!(parsed.wire, ProviderWire::OpenaiChat);
        assert_eq!(
            parsed.base_url.as_deref(),
            Some("https://api.groq.com/openai")
        );
        assert_eq!(parsed.api_key_env, "GROQ_API_KEY");
    }

    #[test]
    fn wire_names_are_kebab_case() {
        assert_eq!(
            entry(r#"{"name":"a","wire":"anthropic","api_key_env":"K"}"#).wire,
            ProviderWire::Anthropic
        );
        assert_eq!(
            entry(r#"{"name":"b","wire":"openai-chat","api_key_env":"K"}"#).wire,
            ProviderWire::OpenaiChat
        );
        assert_eq!(
            entry(r#"{"name":"c","wire":"openai-responses","api_key_env":"K"}"#).wire,
            ProviderWire::OpenaiResponses
        );
    }

    #[test]
    fn base_url_is_optional() {
        assert!(
            entry(r#"{"name":"x","wire":"anthropic","api_key_env":"K"}"#)
                .base_url
                .is_none()
        );
    }

    #[test]
    fn empty_providers_when_no_key() {
        let file: ProviderConfigFile = serde_json::from_str("{}").expect("parse");
        assert!(file.providers.is_empty());
    }

    #[test]
    fn from_entries_rejects_builtin_shadow() {
        let err = ProviderRegistry::from_entries(vec![entry(
            r#"{"name":"claude","wire":"anthropic","api_key_env":"K"}"#,
        )])
        .expect_err("should reject");
        assert!(err.to_string().contains("shadows a built-in"));
    }

    #[test]
    fn from_entries_rejects_duplicate() {
        let err = ProviderRegistry::from_entries(vec![
            entry(r#"{"name":"dup","wire":"anthropic","api_key_env":"K"}"#),
            entry(r#"{"name":"dup","wire":"openai-chat","api_key_env":"K"}"#),
        ])
        .expect_err("should reject");
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn from_entries_rejects_empty_name_and_env() {
        assert!(
            ProviderRegistry::from_entries(vec![entry(
                r#"{"name":"  ","wire":"anthropic","api_key_env":"K"}"#
            )])
            .is_err()
        );
        assert!(
            ProviderRegistry::from_entries(vec![entry(
                r#"{"name":"ok","wire":"anthropic","api_key_env":"  "}"#
            )])
            .is_err()
        );
    }

    #[test]
    fn parse_builtin_aliases() {
        assert_eq!(parse_builtin("claude"), Some(ProviderId::Anthropic));
        assert_eq!(parse_builtin("ANTHROPIC"), Some(ProviderId::Anthropic));
        assert_eq!(parse_builtin("chatgpt"), Some(ProviderId::OpenAi));
        assert_eq!(parse_builtin("openai_responses"), Some(ProviderId::OpenAi));
        assert_eq!(parse_builtin("grok"), Some(ProviderId::Xai));
        assert_eq!(parse_builtin("mystery"), None);
    }

    #[test]
    fn resolves_builtin_with_explicit_key() {
        let registry = ProviderRegistry::default();
        let provider = registry
            .resolve("anthropic", Some("sk-test"))
            .expect("resolve");
        assert_eq!(provider.id(), ProviderId::Anthropic);
    }

    #[test]
    fn resolves_config_provider_with_explicit_key() {
        // openai-responses wire is backed by the OpenAi-id Responses impl; the
        // override key avoids any environment dependency.
        let registry = ProviderRegistry::from_entries(vec![entry(
            r#"{"name":"gw","wire":"openai-responses",
                "base_url":"https://gw.example","api_key_env":"GW_KEY"}"#,
        )])
        .expect("registry");
        let provider = registry.resolve("gw", Some("sk-test")).expect("resolve");
        assert_eq!(provider.id(), ProviderId::OpenAi);
    }

    #[test]
    fn resolves_config_provider_from_env_var() {
        // Unique variable name so this is the only reader/writer of it, keeping
        // the process-global env mutation isolated from other tests.
        let var = "NERVE_TEST_PROVIDER_KEY_FROM_ENV";
        // SAFETY: single-purpose unique key, set and removed within this test.
        unsafe { std::env::set_var(var, "sk-from-env") };
        let registry = ProviderRegistry::from_entries(vec![entry(&format!(
            r#"{{"name":"envgw","wire":"openai-chat",
                "base_url":"https://gw.example","api_key_env":"{var}"}}"#
        ))])
        .expect("registry");
        let provider = registry.resolve("envgw", None).expect("resolve");
        assert_eq!(provider.id(), ProviderId::Xai);
        // SAFETY: see above.
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn unknown_provider_is_error() {
        let registry = ProviderRegistry::default();
        assert!(registry.resolve("nope", Some("k")).is_err());
    }
}
