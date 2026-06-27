//! Locating external agent CLIs and repairing `PATH` for GUI-launched daemons.
//!
//! When the daemon is launched from a macOS GUI (Finder/Dock → launchd) it
//! inherits launchd's minimal `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`), not the
//! user's login-shell `PATH`. The external agent CLIs the delegate seam spawns
//! (`claude` / `codex`) live in user-local bin directories
//! (`~/.local/bin`, Homebrew, `~/.cargo/bin`, …) that are absent from that
//! minimal PATH, so a bare `Command::new("claude")` fails with ENOENT —
//! surfaced as the opaque `failed to spawn `claude``.
//!
//! This module repairs that by augmenting the process `PATH` with the standard
//! user-local bin directories (existing ones only, in a deterministic order).
//! The delegate spawn uses the merged PATH both to (a) resolve the agent program
//! to an absolute path ([`resolve_program`]) and (b) hand the spawned agent a
//! usable `PATH` for its own subtools ([`child_path`]). The merge is computed
//! once and cached.
//!
//! Deliberately shell-free: it only reads `PATH`/`HOME` and probes the
//! filesystem for existing directories/executables — no login shell is spawned,
//! so the behaviour stays deterministic and unit-testable.

use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Resolve a delegate agent program name to an absolute path on the repaired
/// PATH. Returns `None` when `program` is empty, already contains a path
/// separator (the caller then uses it verbatim), or no executable by that name
/// exists on the merged PATH.
pub(crate) fn resolve_program(program: &str) -> Option<PathBuf> {
    if program.is_empty() || program.contains('/') {
        return None;
    }
    find_executable(merged_path(), program)
}

/// The repaired `PATH` value to hand a delegated child as its own `PATH`, so the
/// agent can find git / language toolchains even under a minimal GUI-inherited
/// environment.
pub(crate) fn child_path() -> OsString {
    merged_path().clone()
}

/// The merged search PATH (process `PATH` ++ standard user-local bin dirs),
/// computed once. See the module docs for why the augmentation is needed.
fn merged_path() -> &'static OsString {
    static MERGED: OnceLock<OsString> = OnceLock::new();
    MERGED.get_or_init(|| {
        let base = env::var_os("PATH").unwrap_or_default();
        merge_path(&base, &standard_bin_dirs())
    })
}

/// Standard user-local bin directories a login shell typically adds but a macOS
/// GUI launch omits. `$HOME`-relative entries are joined against `HOME`; only
/// directories that actually exist are kept, in a stable order.
fn standard_bin_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
    {
        for rel in [
            ".local/bin",
            ".cargo/bin",
            ".bun/bin",
            ".deno/bin",
            ".npm-global/bin",
            "go/bin",
            "Library/pnpm",
        ] {
            dirs.push(home.join(rel));
        }
    }
    for abs in ["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin"] {
        dirs.push(PathBuf::from(abs));
    }
    dirs.retain(|dir| dir.is_dir());
    dirs
}

/// Append `extra` directories to the `base` PATH value, preserving order and
/// dropping duplicates (a directory already present in `base` is not re-added).
fn merge_path(base: &OsStr, extra: &[PathBuf]) -> OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for dir in env::split_paths(base) {
        if !dir.as_os_str().is_empty() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    for dir in extra {
        if !dirs.contains(dir) {
            dirs.push(dir.clone());
        }
    }
    env::join_paths(&dirs).unwrap_or_else(|_| base.to_os_string())
}

/// Find the first executable file named `program` across the `:`-joined `path`.
fn find_executable(path: &OsStr, program: &str) -> Option<PathBuf> {
    env::split_paths(path)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_path_dedups_and_appends_in_order() {
        let base = env::join_paths(["/usr/bin", "/bin"]).unwrap();
        let extra = vec![
            PathBuf::from("/usr/bin"), // already present → skipped
            PathBuf::from("/opt/homebrew/bin"),
        ];
        let merged = merge_path(&base, &extra);
        let dirs: Vec<PathBuf> = env::split_paths(&merged).collect();
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
                PathBuf::from("/opt/homebrew/bin"),
            ]
        );
    }

    #[test]
    fn merge_path_keeps_base_when_no_extra() {
        let base = env::join_paths(["/usr/bin", "/bin"]).unwrap();
        assert_eq!(merge_path(&base, &[]), base);
    }

    #[test]
    fn resolve_program_skips_paths_with_separator_or_empty() {
        assert_eq!(resolve_program("/abs/claude"), None);
        assert_eq!(resolve_program("dir/claude"), None);
        assert_eq!(resolve_program(""), None);
    }

    #[cfg(unix)]
    #[test]
    fn find_executable_finds_exec_and_skips_nonexec_and_missing() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        // An executable file.
        let exe = dir.path().join("toolx");
        std::fs::write(&exe, b"#!/bin/sh\n").expect("write exe");
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        // A non-executable file (no exec bits).
        let plain = dir.path().join("plain");
        std::fs::write(&plain, b"data").expect("write plain");
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        let path = dir.path().as_os_str().to_os_string();
        assert_eq!(find_executable(&path, "toolx"), Some(exe));
        assert_eq!(find_executable(&path, "plain"), None); // present but not executable
        assert_eq!(find_executable(&path, "absent"), None); // missing
    }
}
