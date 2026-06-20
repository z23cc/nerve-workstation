//! DA-6: the codex MCP allowlist for delegated codex sessions.
//!
//! When Nerve delegates to codex, both the persistent `codex app-server`
//! ([`delegate_session_codex`](crate::delegate_session_codex)) and the one-shot
//! `codex exec` ([`delegate_runtime`](crate::delegate_runtime)) boot **every**
//! `[mcp_servers.<name>]` configured in the user's `~/.codex/config.toml` before
//! any work starts (~15-20s for a dozen servers). A delegated subtask rarely needs
//! those, so DA-6 makes delegated codex **disable all MCP by default** and re-enable
//! only an explicit allowlist.
//!
//! The mechanism is codex's own per-invocation override: `-c
//! mcp_servers.<name>.enabled=false` makes codex skip that server's boot (verified
//! against chrome-devtools / xcodebuildmcp). For each server in the *disabled set*
//! (discovered servers − allowlist) we append that flag to the codex argv, sorted
//! for deterministic/golden-stable output. An empty/absent allowlist disables all.
//!
//! ## Scope: only `[mcp_servers.*]` are toggleable
//!
//! codex also boots four built-in plugin/app servers — `context-mode`, `codex_apps`,
//! `codex-security`, `computer-use` — that are **not** `[mcp_servers]` entries and do
//! **not** respond to the `mcp_servers.*` override. They are out of scope for this
//! allowlist: we never emit flags for them and [`list_agents`](crate::delegate)
//! marks them non-toggleable. See [`NON_TOGGLEABLE_PLUGIN_SERVERS`].

use crate::delegate_runtime::DelegateAgent;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// codex's built-in plugin/app servers. These are NOT `[mcp_servers.<name>]`
/// entries and do not respond to the `-c mcp_servers.<name>.enabled=false`
/// override, so the DA-6 allowlist cannot toggle them — they are surfaced
/// (read-only) by `list_agents` purely so a client knows they exist and are
/// non-toggleable.
pub(crate) const NON_TOGGLEABLE_PLUGIN_SERVERS: &[&str] = &[
    "context-mode",
    "codex_apps",
    "codex-security",
    "computer-use",
];

/// Resolve the codex config file path: `$CODEX_HOME/config.toml`, defaulting to
/// `~/.codex/config.toml`. Returns `None` only when neither `CODEX_HOME` nor a home
/// directory can be determined.
fn codex_config_path() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(home).join("config.toml"));
    }
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".codex").join("config.toml"))
}

/// Discover the configured codex MCP server names from `~/.codex/config.toml` (or
/// `$CODEX_HOME/config.toml`). A missing/unreadable file yields `[]`.
///
/// This is the toggleable set the allowlist operates over. The plugin/app servers
/// ([`NON_TOGGLEABLE_PLUGIN_SERVERS`]) are never included.
#[must_use]
pub(crate) fn discover_codex_mcp_servers() -> Vec<String> {
    #[cfg(test)]
    if let Some(names) = test_override::take() {
        return names;
    }
    let Some(path) = codex_config_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    parse_mcp_server_names(&text)
}

/// Test-only discovery override: a thread-local set of "discovered" codex MCP
/// servers. Tests that exercise an integration path (the `delegate_agent` tool, the
/// `delegate.start` job, `list_agents`) install a value here so discovery is
/// deterministic **without** mutating process-wide env (`std::env` writes race with
/// concurrent readers under Rust 2024). Thread-local => each test thread is isolated.
#[cfg(test)]
pub(crate) mod test_override {
    use std::cell::RefCell;

    thread_local! {
        static DISCOVERED: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }

    /// Run `f` with discovery on this thread pinned to `servers`, restoring the prior
    /// override afterward. Every [`super::discover_codex_mcp_servers`] call inside `f`
    /// returns this set, so the integration paths are deterministic and env-free.
    pub(crate) fn with<T>(servers: &[&str], f: impl FnOnce() -> T) -> T {
        let value: Vec<String> = servers.iter().map(|s| (*s).to_string()).collect();
        let prev = DISCOVERED.with(|cell| cell.borrow_mut().replace(value));
        let out = f();
        DISCOVERED.with(|cell| *cell.borrow_mut() = prev);
        out
    }

    /// The current thread's pinned discovery set, if any.
    pub(super) fn take() -> Option<Vec<String>> {
        DISCOVERED.with(|cell| cell.borrow().clone())
    }
}

/// Scan TOML text for `[mcp_servers.<name>]` table headers, returning the distinct
/// `<name>` values in first-seen order. Sub-tables like `[mcp_servers.x.env]` or
/// `[mcp_servers.x.http_headers]` name the *parent* server (`x`), never a server of
/// their own. A minimal line scan (no `toml` dependency) is enough: server names are
/// always bare table headers.
fn parse_mcp_server_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let Some(name) = mcp_server_name_from_header(line) else {
            continue;
        };
        if seen.insert(name.clone()) {
            names.push(name);
        }
    }
    names
}

/// Extract the server name from a single line if it is an `[mcp_servers.<name>]`
/// (or `[mcp_servers.<name>.<sub>]`) table header. Returns the *server* name (the
/// first segment after `mcp_servers.`), so sub-tables collapse onto their parent.
/// Ignores comments, inline-table `[[…]]` arrays, and anything not under
/// `mcp_servers`.
fn mcp_server_name_from_header(line: &str) -> Option<String> {
    let trimmed = line.trim();
    // A table header is `[...]`; reject array-of-tables `[[...]]` and comments.
    let inner = trimmed.strip_prefix('[')?.strip_suffix(']')?;
    if inner.starts_with('[') || inner.ends_with(']') {
        return None;
    }
    let rest = inner.strip_prefix("mcp_servers.")?;
    // The server name is the first dotted segment; a quoted segment keeps its
    // quotes stripped. `[mcp_servers.x.env]` -> `x`.
    let segment = rest.split('.').next()?.trim();
    let name = segment.trim_matches('"').trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// The set of codex MCP servers to **disable** for a delegated run: the discovered
/// servers minus the effective allowlist. Sorted + de-duplicated for deterministic
/// argv. Allowlist entries that name no discovered server are simply ignored (they
/// disable nothing).
#[must_use]
pub(crate) fn disabled_servers(discovered: &[String], allowlist: &[String]) -> Vec<String> {
    let allowed: BTreeSet<&str> = allowlist.iter().map(String::as_str).collect();
    let mut disabled: Vec<String> = discovered
        .iter()
        .filter(|name| !allowed.contains(name.as_str()))
        .cloned()
        .collect();
    disabled.sort();
    disabled.dedup();
    disabled
}

/// The `-c mcp_servers.<name>.enabled=false` argv flags for a disabled set, in the
/// `disabled`-set order (callers pass an already-sorted set from
/// [`disabled_servers`], so the flags are deterministic). Each server contributes a
/// `-c` / `key=false` pair.
#[must_use]
pub(crate) fn disable_flags(disabled: &[String]) -> Vec<String> {
    let mut flags = Vec::with_capacity(disabled.len() * 2);
    for name in disabled {
        flags.push("-c".to_string());
        flags.push(format!("mcp_servers.{name}.enabled=false"));
    }
    flags
}

/// Convenience for the argv builders: discover the configured servers, compute the
/// disabled set against `allowlist`, and render the `-c …=false` flags. The
/// discovery is read at build time so a config change is picked up on the next
/// delegated run.
#[must_use]
pub(crate) fn codex_mcp_disable_flags(allowlist: &[String]) -> Vec<String> {
    let discovered = discover_codex_mcp_servers();
    disable_flags(&disabled_servers(&discovered, allowlist))
}

/// The codex MCP-disable argv flags for a delegated run, applying the DA-6 policy.
/// Only codex is affected; any other agent yields an empty set (the flags are
/// codex-only). The **effective allowlist** is the per-call `mcp_enable`
/// (`Some(list)`, including an empty list = disable all) overriding the persisted
/// `[delegate.codex] mcp_enable` config (`None` = use config). Shared by the
/// `delegate.start` job path and the `delegate_agent` tool path so both apply the
/// same policy.
#[must_use]
pub(crate) fn delegate_disable_flags(
    resolved: DelegateAgent,
    mcp_enable: Option<Vec<String>>,
) -> Vec<String> {
    if resolved != DelegateAgent::Codex {
        return Vec::new();
    }
    let allowlist = mcp_enable.unwrap_or_else(crate::runconfig::codex_mcp_allowlist);
    codex_mcp_disable_flags(&allowlist)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_headers_and_ignores_subtables() {
        let toml = r#"
# top-level codex config
model = "o3"

[mcp_servers.chrome-devtools]
command = "chrome-devtools-mcp"

[mcp_servers.xcodebuildmcp]
command = "xcodebuildmcp"
[mcp_servers.xcodebuildmcp.env]
FOO = "bar"

[mcp_servers."quoted-name"]
command = "x"

# a non-mcp table must be ignored
[history]
persistence = "save-all"
"#;
        let names = parse_mcp_server_names(toml);
        assert_eq!(
            names,
            vec![
                "chrome-devtools".to_string(),
                "xcodebuildmcp".to_string(),
                "quoted-name".to_string(),
            ],
            "sub-tables (.env) collapse onto the parent; non-mcp tables ignored"
        );
    }

    #[test]
    fn header_parser_rejects_non_mcp_and_arrays_and_comments() {
        assert_eq!(mcp_server_name_from_header("[history]"), None);
        assert_eq!(mcp_server_name_from_header("# [mcp_servers.x]"), None);
        assert_eq!(mcp_server_name_from_header("[[mcp_servers]]"), None);
        assert_eq!(mcp_server_name_from_header("command = \"x\""), None);
        assert_eq!(
            mcp_server_name_from_header("  [mcp_servers.indented]  "),
            Some("indented".to_string())
        );
    }

    #[test]
    fn empty_text_discovers_nothing() {
        assert!(parse_mcp_server_names("").is_empty());
        assert!(parse_mcp_server_names("model = \"o3\"\n").is_empty());
    }

    #[test]
    fn disabled_set_is_discovered_minus_allowlist_sorted() {
        let discovered = vec!["c".to_string(), "a".to_string(), "b".to_string()];
        // Allowlist {a} -> disable {b, c}, sorted.
        assert_eq!(
            disabled_servers(&discovered, &["a".to_string()]),
            vec!["b".to_string(), "c".to_string()]
        );
        // Empty allowlist -> disable everything.
        assert_eq!(
            disabled_servers(&discovered, &[]),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        // Full allowlist -> disable nothing.
        assert!(
            disabled_servers(
                &discovered,
                &["a".to_string(), "b".to_string(), "c".to_string()]
            )
            .is_empty()
        );
    }

    #[test]
    fn allowlist_entries_for_unknown_servers_disable_nothing() {
        let discovered = vec!["a".to_string()];
        // "ghost" names no discovered server; "a" is allowed, so nothing is disabled.
        assert!(disabled_servers(&discovered, &["a".to_string(), "ghost".to_string()]).is_empty());
        // No allowlist match for "a" -> it is disabled; "ghost" is irrelevant.
        assert_eq!(
            disabled_servers(&discovered, &["ghost".to_string()]),
            vec!["a".to_string()]
        );
    }

    #[test]
    fn disable_flags_emit_sorted_c_pairs() {
        let flags = disable_flags(&["b".to_string(), "c".to_string()]);
        assert_eq!(
            flags,
            vec![
                "-c".to_string(),
                "mcp_servers.b.enabled=false".to_string(),
                "-c".to_string(),
                "mcp_servers.c.enabled=false".to_string(),
            ]
        );
        assert!(disable_flags(&[]).is_empty());
    }

    #[test]
    fn plugin_servers_are_not_toggleable_and_named() {
        // The four built-in plugin/app servers are documented as non-toggleable;
        // they are never discovered (not [mcp_servers]) so they never get flags.
        assert_eq!(NON_TOGGLEABLE_PLUGIN_SERVERS.len(), 4);
        let toml = "[mcp_servers.context-mode]\ncommand = \"x\"\n";
        // Even if a user *did* declare one under [mcp_servers], that's their own
        // entry and is toggleable; the built-in plugin of the same name is separate
        // and out of scope. The parser only reports declared [mcp_servers] names.
        assert_eq!(
            parse_mcp_server_names(toml),
            vec!["context-mode".to_string()]
        );
    }
}
