//! Check-run **emitters** for the merge gate (`docs/designs/trust-substrate.md` §8 L5):
//! the side-effecting sinks that mirror a [`GateOutcome`] onto a code-host check surface
//! (GitHub Checks API via `gh`, GitLab Commit Status API via `curl`) or do nothing (the
//! authoritative exit code alone). Split out of `gate/mod.rs` so the gate decision logic
//! stays under the file-size cap; the emitters only **mirror** the exit code — none can
//! ever fabricate a pass (INV-R1).

use anyhow::{Context, Result, anyhow};
use nerve_core::receipt_gate::GateOutcome;
use std::process::Command;

/// Side-effecting sink that posts a merge-gate decision to a code-host check surface.
/// The default impl ([`NoopEmitter`]) does nothing — the exit code is authoritative —
/// so a deployed merge App or a CI step both work without code change. This is the
/// deferred-infra seam (trust-substrate §8): a GitHub App / GitLab status can replace
/// the shelled `gh` path without touching the gate logic.
pub(crate) trait CheckRunEmitter {
    /// Post (or skip) a check run for `outcome` against `sha` in `repo`. Best-effort:
    /// a posting failure is reported but never overrides the authoritative exit code.
    fn emit(&self, repo: &str, sha: &str, outcome: &GateOutcome) -> Result<()>;
}

/// The no-op emitter: the exit code alone is the gate. Used when `--emit none`.
pub(crate) struct NoopEmitter;

impl CheckRunEmitter for NoopEmitter {
    fn emit(&self, _repo: &str, _sha: &str, _outcome: &GateOutcome) -> Result<()> {
        Ok(())
    }
}

/// Posts a GitHub check run by shelling `gh api` (the deferred-infra default until a
/// first-party GitHub App is deployed). The `gh` CLI carries the auth; we only build
/// the Checks-API request body from the pure [`GateOutcome`].
pub(crate) struct GhCheckRunEmitter {
    /// The check run's display name (the row shown on the PR).
    pub(crate) name: String,
}

impl Default for GhCheckRunEmitter {
    fn default() -> Self {
        Self {
            name: "nerve/verification-receipt".to_string(),
        }
    }
}

impl GhCheckRunEmitter {
    /// The `gh api` argument vector that POSTs a check run for `outcome`. Pure (no IO)
    /// so it is unit-testable without invoking `gh`.
    pub(crate) fn gh_args(&self, repo: &str, sha: &str, outcome: &GateOutcome) -> Vec<String> {
        vec![
            "api".to_string(),
            "--method".to_string(),
            "POST".to_string(),
            format!("repos/{repo}/check-runs"),
            "-f".to_string(),
            format!("name={}", self.name),
            "-f".to_string(),
            format!("head_sha={sha}"),
            "-f".to_string(),
            "status=completed".to_string(),
            "-f".to_string(),
            format!("conclusion={}", outcome.conclusion),
            "-f".to_string(),
            format!(
                "output[title]=Nerve verification receipt: {}",
                outcome.conclusion
            ),
            "-f".to_string(),
            format!("output[summary]={}", outcome.summary),
        ]
    }
}

impl CheckRunEmitter for GhCheckRunEmitter {
    fn emit(&self, repo: &str, sha: &str, outcome: &GateOutcome) -> Result<()> {
        let status = Command::new("gh")
            .args(self.gh_args(repo, sha, outcome))
            .status()
            .context("failed to spawn `gh` (is the GitHub CLI installed and authed?)")?;
        if status.success() {
            Ok(())
        } else {
            Err(anyhow!("`gh api` exited with status {status}"))
        }
    }
}

/// The default GitLab API v4 base, used when `CI_API_V4_URL` is not set (i.e. running
/// outside a GitLab pipeline against gitlab.com).
pub(crate) const GITLAB_DEFAULT_API_BASE: &str = "https://gitlab.com/api/v4";

/// Posts a GitLab **commit status** by shelling `curl` to the Commit Status API (the
/// GitLab counterpart of [`GhCheckRunEmitter`]; deferred-infra default until a
/// first-party GitLab integration is deployed). The status only **mirrors** the
/// authoritative exit code: it is `success` IFF the receipt cleared (exit 0), else
/// `failed` — an un-cleared verdict never posts a pass (INV-R1). The auth token is read
/// from the environment inside [`emit`](GitLabStatusEmitter::emit) only and is never
/// part of the pure [`curl_args`](GitLabStatusEmitter::curl_args), so it cannot leak
/// into a logged argv or a test fixture.
#[derive(Default)]
pub(crate) struct GitLabStatusEmitter;

impl GitLabStatusEmitter {
    /// The GitLab commit-status `state` that mirrors `outcome` (INV-R1): `success` IFF
    /// the receipt cleared (`exit_code == 0`), otherwise `failed` — so Failed,
    /// Inconclusive, and Error all block the pipeline and an un-cleared verdict is never
    /// posted as a pass. The real reason rides in the status `description`.
    pub(crate) fn state_for(outcome: &GateOutcome) -> &'static str {
        if outcome.exit_code == 0 {
            "success"
        } else {
            "failed"
        }
    }

    /// The `curl` argument vector that POSTs a commit status for `outcome`. Pure (no IO,
    /// **no token** — the auth header is added in [`emit`](Self::emit) only) so it is
    /// unit-testable without invoking `curl` and cannot leak a secret into a fixture or
    /// a logged argv.
    pub(crate) fn curl_args(
        api_base: &str,
        project: &str,
        sha: &str,
        outcome: &GateOutcome,
    ) -> Vec<String> {
        let project = urlencode(project);
        let url = format!("{api_base}/projects/{project}/statuses/{sha}");
        vec![
            "-sS".to_string(),
            "--fail".to_string(),
            "--request".to_string(),
            "POST".to_string(),
            "--data-urlencode".to_string(),
            format!("state={}", Self::state_for(outcome)),
            "--data-urlencode".to_string(),
            "name=nerve-gate".to_string(),
            "--data-urlencode".to_string(),
            format!("description={}", outcome.summary),
            url,
        ]
    }
}

impl CheckRunEmitter for GitLabStatusEmitter {
    fn emit(&self, repo: &str, sha: &str, outcome: &GateOutcome) -> Result<()> {
        let api_base =
            std::env::var("CI_API_V4_URL").unwrap_or_else(|_| GITLAB_DEFAULT_API_BASE.to_string());
        // Token read from env here only — never in `curl_args` (secret safety).
        let (header_name, token) = match std::env::var("GITLAB_TOKEN") {
            Ok(token) if !token.is_empty() => ("PRIVATE-TOKEN", token),
            _ => (
                "JOB-TOKEN",
                std::env::var("CI_JOB_TOKEN").map_err(|_| {
                    anyhow!("no GitLab auth: set GITLAB_TOKEN (PRIVATE-TOKEN) or CI_JOB_TOKEN")
                })?,
            ),
        };
        let status = Command::new("curl")
            .arg("--header")
            .arg(format!("{header_name}: {token}"))
            .args(Self::curl_args(&api_base, repo, sha, outcome))
            .status()
            .context("failed to spawn `curl` (is it installed?)")?;
        if status.success() {
            Ok(())
        } else {
            Err(anyhow!(
                "`curl` to the GitLab Commit Status API exited with status {status}"
            ))
        }
    }
}

/// Minimal percent-encoding for a GitLab project id path segment (so `group/project`
/// becomes `group%2Fproject`). Numeric ids pass through unchanged. Encodes the
/// path-unsafe characters GitLab project paths can contain; ASCII alnum, `-`, `_`, `.`
/// stay literal.
fn urlencode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Pick the [`CheckRunEmitter`] for `--emit`: `none` (exit-code-only), `gh` (GitHub
/// Checks API via `gh`), or `gitlab` (GitLab Commit Status API via `curl`). Every
/// emitter only mirrors the authoritative exit code — none can fabricate a pass (INV-R1).
pub(crate) fn select_emitter(emit: &str) -> Result<Box<dyn CheckRunEmitter>> {
    match emit {
        "none" => Ok(Box::new(NoopEmitter)),
        "gh" => Ok(Box::new(GhCheckRunEmitter::default())),
        "gitlab" => Ok(Box::new(GitLabStatusEmitter)),
        other => Err(anyhow!(
            "unknown --emit `{other}` (expected: none, gh, gitlab)"
        )),
    }
}
