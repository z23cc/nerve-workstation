//! Execution-grounded verdict vocabulary — the **L2 re-verifier** data shapes
//! (`docs/designs/trust-substrate.md` §3 L2, §8). After a captured [`crate::Run`]
//! has been replayed, Nerve re-runs the **org's own checks** (tests, typecheck,
//! build, lint, …) inside the recorded closure and seals the outcome into a
//! [`Verdict`]. The verdict's verdict is *borrowed* from the org's bar — Nerve is
//! the court reporter, never the judge (INV-R1): a [`Verdict`] proves only that the
//! checks were run reproducibly and cleared (or did not clear) the org's own bar.
//!
//! This module is the **keystone vocabulary** of the trust substrate: the ledger
//! (L1), the portable signed receipt (L4), and the merge-gate (L5) all *import*
//! [`CheckKind`] / [`CheckResult`] / [`VerdictStatus`] from here rather than
//! redefining them, so a check's classification and a verdict's status mean exactly
//! one thing across the whole stack.
//!
//! These are **pure, transport-neutral serde data** (INV-R5) with **no behavior** —
//! every hash field is a plain `String`. The pure canonicalization + SHA-256 hashing
//! that *fills* `output_hash` / `checkspec_hash` / `closure_digest` / `verdict_hash`
//! lives in `nerve-core::verdict` (INV-R2: the hashing is pure and golden-tested),
//! never here. **No floats** appear in any hashed type — durations are `u64`
//! milliseconds and counts are `u32`/`u64` — so the canonical JSON is byte-stable
//! and the types derive `Eq` (exact golden snapshots, no precision nondeterminism).

#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// On-disk + on-wire verdict schema version. Bumped only for additive,
/// backward-compatible changes to the [`Verdict`] shape; a reader rejects a record
/// from a newer major it cannot understand rather than silently dropping fields.
pub const VERDICT_SCHEMA_VERSION: u32 = 1;

/// What kind of check a [`CheckResult`] reports — the org-bar dimension being
/// re-verified. Serialized `snake_case` (`{"kind":"test"}`). `Copy` because it is a
/// fieldless tag. The set is execution-grounded — only check classes the runner can
/// actually drive today. Additive: new kinds may be appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    /// The org's test suite (or a named subset of it).
    Test,
    /// A type-checker pass (e.g. `tsc`, `mypy`, `cargo check`).
    Typecheck,
    /// A build / compile step.
    Build,
    /// A linter / formatter gate.
    Lint,
    /// A property-based test run.
    Property,
    /// A mutation-testing run.
    Mutation,
    /// A contamination / leakage check (e.g. secrets, train/test bleed).
    Contamination,
}

/// The status of a single re-run [`CheckResult`]. Serialized `snake_case`
/// (`{"status":"pass"}`). `Copy` because it is a fieldless tag. `Flaky` means the
/// check did not produce a stable result across reruns (it both passed and failed),
/// which a verdict treats as inconclusive rather than a clean pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// The check passed reproducibly across all reruns.
    Pass,
    /// The check failed reproducibly across all reruns.
    Fail,
    /// The check produced inconsistent results across reruns (not reproducible).
    Flaky,
    /// The check could not be executed (harness/infrastructure error, not a
    /// pass/fail signal from the code under test).
    Error,
}

/// The aggregate verdict of a whole [`Verdict`] — whether the run cleared the org's
/// bar. Serialized `snake_case` (`{"status":"passed"}`). `Copy` because it is a
/// fieldless tag. `Inconclusive` is the honest "no bar exercised / not reproducible"
/// state (e.g. an empty checkspec or a flaky required check) — distinct from
/// `Failed` (the bar was exercised and not met) and `Error` (the harness broke).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum VerdictStatus {
    /// Every required check passed reproducibly — the org's bar was cleared.
    Passed,
    /// At least one required check failed reproducibly — the bar was not met.
    Failed,
    /// No bar was exercised, or a required check was flaky/non-reproducible.
    Inconclusive,
    /// The verification harness itself errored and could not render a verdict.
    Error,
}

/// One check's outcome inside a [`Verdict`]. `name` is the human-facing check label
/// (e.g. `"cargo test"`); `kind` classifies it; `status` is the re-run result.
/// `reproducible` records whether the check yielded the same result across reruns.
/// `runs` / `passed` count the rerun attempts and how many passed (so a verifier can
/// re-derive flakiness). `output_hash` is the content address of the captured check
/// output (filled by `nerve-core`, `""` until sealed). `duration_ms` is integer
/// milliseconds (never a float) so the hashed canonical bytes are stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct CheckResult {
    /// Human-facing check label (e.g. the command name).
    pub name: String,
    /// Which org-bar dimension this check exercises.
    pub kind: CheckKind,
    /// The re-run status (pass/fail/flaky/error).
    pub status: CheckStatus,
    /// Whether the check produced the same result across all reruns.
    pub reproducible: bool,
    /// The process exit code when the check ran as a subprocess and one was observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Whether the check was killed by the wall-clock timeout.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
    /// Integer milliseconds the check took (host metadata; never a float).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub duration_ms: u64,
    /// Content address of the captured check output (`""` until sealed).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output_hash: String,
    /// How many rerun attempts were made.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub runs: u32,
    /// How many of the [`Self::runs`] attempts passed.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub passed: u32,
}

/// A sealed re-verification of a captured [`crate::Run`] — the L2 unit of trust. The
/// [`Self::checks`] are the org-bar checks re-run inside the recorded closure;
/// [`Self::status`] is the borrowed aggregate verdict. `verdict_id` is the content
/// address of the verdict's *identity* (run + diff + checkspec + closure + checks),
/// assigned at seal; `verdict_hash` is the digest committing to the sealed record
/// (both filled by `nerve-core`, `""` until sealed). `checkspec_hash` pins the
/// definition of *which* checks were run, and `closure_digest` pins the environment
/// they ran in, so a verifier can confirm the bar and the closure independently.
/// `verified_at_ms` is host wall-clock metadata, **excluded from the hashed bytes**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Verdict {
    /// Verdict schema version (see [`VERDICT_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Content address of the verdict's identity (filled at seal).
    pub verdict_id: String,
    /// The captured run this verdict re-verifies.
    pub run_id: String,
    /// Content address of the diff under verification, when one is bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_hash: Option<String>,
    /// The borrowed aggregate verdict over [`Self::checks`].
    pub status: VerdictStatus,
    /// Content address pinning *which* checks were run (the checkspec).
    pub checkspec_hash: String,
    /// Content address pinning the environment the checks ran in.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub closure_digest: String,
    /// The org-bar checks re-run inside the recorded closure.
    pub checks: Vec<CheckResult>,
    /// Host wall-clock millis at seal (metadata; excluded from hashed bytes).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub verified_at_ms: u64,
    /// Digest committing to the sealed record (filled at seal).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub verdict_hash: String,
}

/// `skip_serializing_if` predicate for `u64` count/duration fields: omit when zero
/// so a default record round-trips byte-identically.
#[allow(clippy::trivially_copy_pass_by_ref)] // serde requires a `&T` predicate.
fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// `skip_serializing_if` predicate for `u32` count fields: omit when zero.
#[allow(clippy::trivially_copy_pass_by_ref)] // serde requires a `&T` predicate.
fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_kind_tags_are_snake_case() {
        let cases = [
            (CheckKind::Test, "test"),
            (CheckKind::Typecheck, "typecheck"),
            (CheckKind::Build, "build"),
            (CheckKind::Lint, "lint"),
            (CheckKind::Property, "property"),
            (CheckKind::Mutation, "mutation"),
            (CheckKind::Contamination, "contamination"),
        ];
        for (kind, tag) in cases {
            let value = serde_json::to_value(kind).expect("kind json");
            assert_eq!(value, serde_json::Value::String(tag.into()));
        }
    }

    #[test]
    fn check_status_tags_are_snake_case() {
        let cases = [
            (CheckStatus::Pass, "pass"),
            (CheckStatus::Fail, "fail"),
            (CheckStatus::Flaky, "flaky"),
            (CheckStatus::Error, "error"),
        ];
        for (status, tag) in cases {
            let value = serde_json::to_value(status).expect("status json");
            assert_eq!(value, serde_json::Value::String(tag.into()));
        }
    }

    #[test]
    fn verdict_status_tags_are_snake_case() {
        let cases = [
            (VerdictStatus::Passed, "passed"),
            (VerdictStatus::Failed, "failed"),
            (VerdictStatus::Inconclusive, "inconclusive"),
            (VerdictStatus::Error, "error"),
        ];
        for (status, tag) in cases {
            let value = serde_json::to_value(status).expect("verdict status json");
            assert_eq!(value, serde_json::Value::String(tag.into()));
        }
    }

    #[test]
    fn check_result_omits_defaults_and_round_trips() {
        let check = CheckResult {
            name: "cargo test".into(),
            kind: CheckKind::Test,
            status: CheckStatus::Pass,
            reproducible: true,
            exit_code: None,
            timed_out: false,
            duration_ms: 0,
            output_hash: String::new(),
            runs: 0,
            passed: 0,
        };
        let value = serde_json::to_value(&check).expect("check json");
        assert_eq!(value["name"], "cargo test");
        assert_eq!(value["kind"], "test");
        assert_eq!(value["status"], "pass");
        assert_eq!(value["reproducible"], true);
        // All optional/defaulted fields are skipped when empty.
        assert!(value.get("exit_code").is_none());
        assert!(value.get("timed_out").is_none());
        assert!(value.get("duration_ms").is_none());
        assert!(value.get("output_hash").is_none());
        assert!(value.get("runs").is_none());
        assert!(value.get("passed").is_none());
        let back: CheckResult = serde_json::from_value(value).expect("round-trip");
        assert_eq!(back, check);
    }

    #[test]
    fn check_result_serializes_present_fields_and_round_trips() {
        let check = CheckResult {
            name: "cargo build".into(),
            kind: CheckKind::Build,
            status: CheckStatus::Fail,
            reproducible: true,
            exit_code: Some(101),
            timed_out: true,
            duration_ms: 4200,
            output_hash: "abcd".into(),
            runs: 3,
            passed: 1,
        };
        let value = serde_json::to_value(&check).expect("check json");
        assert_eq!(value["exit_code"], 101);
        assert_eq!(value["timed_out"], true);
        assert_eq!(value["duration_ms"], 4200);
        assert_eq!(value["output_hash"], "abcd");
        assert_eq!(value["runs"], 3);
        assert_eq!(value["passed"], 1);
        let back: CheckResult = serde_json::from_value(value).expect("round-trip");
        assert_eq!(back, check);
    }

    #[test]
    fn verdict_round_trips_and_defaults_are_tolerant() {
        let verdict = Verdict {
            schema_version: VERDICT_SCHEMA_VERSION,
            verdict_id: "vid".into(),
            run_id: "run-1".into(),
            diff_hash: Some("dh".into()),
            status: VerdictStatus::Passed,
            checkspec_hash: "spec".into(),
            closure_digest: "closure".into(),
            checks: vec![CheckResult {
                name: "cargo test".into(),
                kind: CheckKind::Test,
                status: CheckStatus::Pass,
                reproducible: true,
                exit_code: Some(0),
                timed_out: false,
                duration_ms: 10,
                output_hash: "oh".into(),
                runs: 2,
                passed: 2,
            }],
            verified_at_ms: 1234,
            verdict_hash: "vh".into(),
        };
        let value = serde_json::to_value(&verdict).expect("verdict json");
        let back: Verdict = serde_json::from_value(value).expect("round-trip");
        assert_eq!(back, verdict);

        // A minimal record deserializes, with additive/defaulted fields falling
        // back to their defaults — forward tolerance.
        let minimal: Verdict = serde_json::from_value(serde_json::json!({
            "schema_version": VERDICT_SCHEMA_VERSION,
            "verdict_id": "v",
            "run_id": "r",
            "status": "inconclusive",
            "checkspec_hash": "s",
            "checks": [],
        }))
        .expect("minimal verdict");
        assert_eq!(minimal.diff_hash, None);
        assert_eq!(minimal.status, VerdictStatus::Inconclusive);
        assert_eq!(minimal.closure_digest, "");
        assert!(minimal.checks.is_empty());
        assert_eq!(minimal.verified_at_ms, 0);
        assert_eq!(minimal.verdict_hash, "");
    }
}
