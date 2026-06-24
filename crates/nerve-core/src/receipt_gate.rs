//! Pure, golden-tested **merge-gate decision** for a signed Verification Receipt
//! (`docs/designs/trust-substrate.md` §8 L5, INV-R1). Given a [`Receipt`] this maps
//! its aggregate [`VerdictStatus`] to the tri-state outcome a CI/merge surface
//! consumes: a process **exit code**, a stable **conclusion** label (the vocabulary
//! GitHub Checks / GitLab commit-status expect), and a one-line human **summary**.
//!
//! **Court reporter, not judge (INV-R1).** The gate does not decide whether the code
//! is "correct" — it only borrows the receipt's already-sealed verdict (which itself
//! borrows the org's own tests) and translates it into a transport-neutral decision.
//! `Inconclusive`/`Error` are treated as **neutral** (no bar exercised / harness
//! failure) and surface exit code 2, so an un-cleared change never reads as a pass.
//!
//! This is the determinism boundary's L5 brick: a pure function of the receipt —
//! no IO, no wall-clock, no randomness — so the same receipt yields a byte-identical
//! [`GateOutcome`]. Side-effecting emission (posting a GitHub check run, exiting the
//! process) lives above the kernel in `nerve-workstation` (INV-R2).

// Re-export the verdict vocabulary this gate reads so a kernel consumer can match on
// the same `VerdictStatus` it sees in a `GateOutcome`'s upstream without taking its
// own `nerve-proto` dependency.
pub use nerve_proto::receipt::Receipt;
pub use nerve_proto::verdict::VerdictStatus;

use serde::{Deserialize, Serialize};

/// The tri-state merge-gate decision derived from a receipt's aggregate verdict.
///
/// `exit_code` is authoritative for a CI step (0 pass / 1 fail / 2 neutral);
/// `conclusion` is the stable label a checks API expects (`"success"` / `"failure"`
/// / `"neutral"` / `"error"`); `summary` is a short human-readable rationale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateOutcome {
    /// Process exit code: `0` on cleared, `1` on a cleared-but-failed bar, `2` when
    /// the bar was not exercised (inconclusive) or the harness errored.
    pub exit_code: i32,
    /// Stable conclusion label for a checks API: `success` / `failure` / `neutral`
    /// / `error`.
    pub conclusion: String,
    /// One-line human-readable rationale for the decision.
    pub summary: String,
}

/// Decide the merge-gate outcome for a signed receipt by borrowing its sealed
/// aggregate verdict (INV-R1). Pure: the same receipt always yields the same
/// [`GateOutcome`].
///
/// Mapping (trust-substrate RISK §6): `Passed → (0, "success")`,
/// `Failed → (1, "failure")`, `Inconclusive → (2, "neutral")`, `Error → (2, "error")`.
#[must_use]
pub fn gate_outcome(receipt: &Receipt) -> GateOutcome {
    match receipt.statement.verdict {
        VerdictStatus::Passed => GateOutcome {
            exit_code: 0,
            conclusion: "success".to_string(),
            summary: "verdict passed: the org's required checks cleared on replay".to_string(),
        },
        VerdictStatus::Failed => GateOutcome {
            exit_code: 1,
            conclusion: "failure".to_string(),
            summary: "verdict failed: a required check did not clear the org's bar".to_string(),
        },
        VerdictStatus::Inconclusive => GateOutcome {
            exit_code: 2,
            conclusion: "neutral".to_string(),
            summary: "verdict inconclusive: no required bar was exercised".to_string(),
        },
        VerdictStatus::Error => GateOutcome {
            exit_code: 2,
            conclusion: "error".to_string(),
            summary: "verdict error: the verification harness failed to run".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nerve_proto::receipt::{
        RECEIPT_PREDICATE_TYPE, RECEIPT_SCHEMA_VERSION, Receipt, ReceiptProvenance,
        ReceiptSignature, ReceiptStatement, ReplayManifest,
    };

    fn receipt_with(verdict: VerdictStatus) -> Receipt {
        Receipt {
            schema_version: RECEIPT_SCHEMA_VERSION,
            receipt_id: "rcpt-deadbeef".to_string(),
            statement: ReceiptStatement {
                predicate_type: RECEIPT_PREDICATE_TYPE.to_string(),
                provenance: ReceiptProvenance {
                    run_id: "run-1".to_string(),
                    inputs_hash: "abc".to_string(),
                    toolchain_digest: None,
                    policy_version: None,
                    ledger_ref: None,
                },
                checks: vec![],
                verdict,
                replay_manifest: ReplayManifest {
                    run_schema_version: 2,
                    root_hash: "root".to_string(),
                    event_count: 0,
                    command: None,
                },
                issued_at_ms: 1000,
            },
            signature: ReceiptSignature {
                payload_type: "application/vnd.in-toto+json".to_string(),
                backend: "local-ed25519".to_string(),
                keyid: "k1".to_string(),
                sig: "sig".to_string(),
                public_key: None,
                bundle: None,
            },
        }
    }

    #[test]
    fn passed_maps_to_success_exit_zero() {
        let out = gate_outcome(&receipt_with(VerdictStatus::Passed));
        assert_eq!(out.exit_code, 0);
        assert_eq!(out.conclusion, "success");
        assert!(!out.summary.is_empty());
    }

    #[test]
    fn failed_maps_to_failure_exit_one() {
        let out = gate_outcome(&receipt_with(VerdictStatus::Failed));
        assert_eq!(out.exit_code, 1);
        assert_eq!(out.conclusion, "failure");
    }

    #[test]
    fn inconclusive_maps_to_neutral_exit_two() {
        let out = gate_outcome(&receipt_with(VerdictStatus::Inconclusive));
        assert_eq!(out.exit_code, 2);
        assert_eq!(out.conclusion, "neutral");
    }

    #[test]
    fn error_maps_to_error_exit_two() {
        let out = gate_outcome(&receipt_with(VerdictStatus::Error));
        assert_eq!(out.exit_code, 2);
        assert_eq!(out.conclusion, "error");
    }

    #[test]
    fn outcome_is_deterministic_for_same_receipt() {
        let receipt = receipt_with(VerdictStatus::Passed);
        assert_eq!(gate_outcome(&receipt), gate_outcome(&receipt));
    }

    #[test]
    fn distinct_verdicts_yield_distinct_outcomes() {
        let passed = gate_outcome(&receipt_with(VerdictStatus::Passed));
        let failed = gate_outcome(&receipt_with(VerdictStatus::Failed));
        let inconclusive = gate_outcome(&receipt_with(VerdictStatus::Inconclusive));
        let error = gate_outcome(&receipt_with(VerdictStatus::Error));
        // Every conclusion label is distinct.
        assert_ne!(passed.conclusion, failed.conclusion);
        assert_ne!(inconclusive.conclusion, error.conclusion);
        // Only a clean pass exits 0; an un-cleared change never reads as success.
        assert_eq!(passed.exit_code, 0);
        assert_ne!(failed.exit_code, 0);
        assert_ne!(inconclusive.exit_code, 0);
        assert_ne!(error.exit_code, 0);
        // Neutral and error share exit 2 but differ in conclusion (RISK §6).
        assert_eq!(inconclusive.exit_code, error.exit_code);
    }

    #[test]
    fn gate_outcome_serde_round_trip() {
        let out = gate_outcome(&receipt_with(VerdictStatus::Failed));
        let json = serde_json::to_string(&out).expect("serializes");
        let back: GateOutcome = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(out, back);
        // Field names are stable / snake_case as the integrator's golden expects.
        assert!(json.contains("\"exit_code\""));
        assert!(json.contains("\"conclusion\""));
        assert!(json.contains("\"summary\""));
    }
}
