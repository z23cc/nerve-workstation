//! Pure, golden-tested content-addressing for the L3 policy plane
//! (`docs/designs/trust-substrate.md` §L3, INV-R2). Given the
//! [`nerve_proto::policy`] shapes, this seals a [`PolicyDoc`] under a
//! self-certifying `policy_version` (a SHA-256 over its own body) and
//! content-addresses the policy-decision evidence the gate records.
//!
//! **This is part of the determinism boundary's kernel:** every function here is
//! a pure function of its arguments — no IO, no wall-clock, no randomness — so the
//! same policy in yields a byte-identical sealed [`PolicyDoc`] out, and the same
//! decision yields the same digest. Capture, persistence and the live gate (which
//! DO touch the world) live above the kernel in `nerve-workstation` (INV-R2).
//!
//! SHA-256 (not the non-cryptographic FNV-1a in [`crate::edit`]) is used
//! deliberately: a sealed policy version and a decision digest are audit-trail
//! evidence that portable receipts (L4) may sign.

// Re-export the shapes this module content-addresses so a consumer of the kernel
// seals a `PolicyDoc` and hashes decisions through `nerve_core` alone, without
// taking its own `nerve-proto` dependency.
pub use nerve_proto::policy::{
    Capability, CapabilityRule, EvidenceRequirement, MergeBar, POLICY_SCHEMA_VERSION,
    PolicyDecisionRecord, PolicyDoc,
};
use sha2::{Digest, Sha256};

/// Seal a [`PolicyDoc`] under a self-certifying `policy_version`.
///
/// The version is computed over the *body* of the doc with `policy_version`
/// zeroed, so the stamped version is a stable content address of the policy's
/// substance: identical rules always seal to the identical version, and any
/// change to the capabilities, merge bar, or required evidence changes it. This
/// makes a [`PolicyDecisionRecord`]'s `policy_version` a tamper-evident pin to
/// the exact policy that was in force.
#[must_use]
pub fn seal_policy(mut doc: PolicyDoc) -> PolicyDoc {
    doc.policy_version = String::new();
    let bytes = serde_json::to_vec(&doc).expect("PolicyDoc serializes infallibly");
    doc.policy_version = hex(Sha256::digest(bytes).as_slice());
    doc
}

/// Lowercase-hex SHA-256 of a [`PolicyDecisionRecord`]'s canonical JSON. This is
/// the content address of one allow/deny decision (already pinned to its policy
/// via `policy_version` and to its arguments via `args_hash`), suitable for the
/// L1 evidence ledger's `detail_hash`. Deterministic: every field is a fixed
/// string/enum with no maps and no floats, so `serde_json` emits byte-stable
/// bytes (INV-R2).
#[must_use]
pub fn hash_decision(record: &PolicyDecisionRecord) -> String {
    let bytes = serde_json::to_vec(record).expect("PolicyDecisionRecord serializes infallibly");
    hex(Sha256::digest(bytes).as_slice())
}

/// Lowercase-hex SHA-256 of a tool-call argument object, used to populate
/// [`PolicyDecisionRecord::args_hash`] without retaining (and re-serializing)
/// the raw — possibly sensitive — arguments in the decision evidence.
/// `serde_json::to_vec` over a [`serde_json::Value`] is deterministic for the
/// shapes a tool call produces (object keys are emitted in sorted order), so the
/// same arguments always hash identically.
#[must_use]
pub fn hash_args(args: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(args).expect("Value serializes infallibly");
    hex(Sha256::digest(bytes).as_slice())
}

/// Lowercase-hex encode bytes (no allocation per byte beyond the result string).
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_doc() -> PolicyDoc {
        PolicyDoc {
            schema_version: POLICY_SCHEMA_VERSION,
            policy_version: String::new(),
            capabilities: vec![CapabilityRule {
                tool: "edit".into(),
                action: "write".into(),
                capability: Capability::Write,
                agent: None,
            }],
            merge_bar: MergeBar {
                required_checks: vec!["test".into(), "build".into()],
                expected_checkspec_hash: None,
            },
            required_evidence: vec![EvidenceRequirement {
                kind: "receipt".into(),
            }],
        }
    }

    fn sample_record() -> PolicyDecisionRecord {
        PolicyDecisionRecord {
            schema_version: POLICY_SCHEMA_VERSION,
            policy_version: "deadbeef".into(),
            session_id: "sess-1".into(),
            agent: "codex".into(),
            tool: "edit".into(),
            capability: Capability::Write,
            decision: "allow".into(),
            reason: "matched write rule".into(),
            args_hash: "abc123".into(),
        }
    }

    #[test]
    fn seal_stamps_a_stable_hex_version_and_is_idempotent() {
        let sealed = seal_policy(sample_doc());
        // 64 lowercase hex chars (SHA-256).
        assert_eq!(sealed.policy_version.len(), 64);
        assert!(sealed.policy_version.chars().all(|c| c.is_ascii_hexdigit()));
        // Same body -> same version (deterministic).
        assert_eq!(
            seal_policy(sample_doc()).policy_version,
            sealed.policy_version
        );
        // Re-sealing an already-sealed doc reproduces the same version: seal zeroes
        // the version before hashing, so the stamp does not feed back into itself.
        assert_eq!(
            seal_policy(sealed.clone()).policy_version,
            sealed.policy_version
        );
    }

    #[test]
    fn seal_ignores_the_incoming_version_field() {
        let mut a = sample_doc();
        a.policy_version = "incoming-A".into();
        let mut b = sample_doc();
        b.policy_version = "incoming-B".into();
        // The pre-existing version field is zeroed before hashing, so two docs that
        // differ only in their (about-to-be-replaced) version seal identically.
        assert_eq!(seal_policy(a).policy_version, seal_policy(b).policy_version);
    }

    #[test]
    fn seal_version_changes_when_the_body_changes() {
        let base = seal_policy(sample_doc()).policy_version;

        let mut diff_cap = sample_doc();
        diff_cap.capabilities[0].capability = Capability::Exec;
        assert_ne!(seal_policy(diff_cap).policy_version, base);

        let mut diff_bar = sample_doc();
        diff_bar.merge_bar.required_checks.push("lint".into());
        assert_ne!(seal_policy(diff_bar).policy_version, base);

        let mut diff_evidence = sample_doc();
        diff_evidence.required_evidence.clear();
        assert_ne!(seal_policy(diff_evidence).policy_version, base);

        // Pinning the bar's checkspec identity is part of the sealed (signed) policy body,
        // so it changes the version; a doc that pins none (sample_doc) seals to `base`.
        let mut diff_checkspec = sample_doc();
        diff_checkspec.merge_bar.expected_checkspec_hash = Some("spec-abc".into());
        assert_ne!(seal_policy(diff_checkspec).policy_version, base);
    }

    #[test]
    fn hash_decision_is_stable_and_distinguishes_content() {
        let r = sample_record();
        assert_eq!(
            hash_decision(&r),
            hash_decision(&r),
            "same record -> same hash"
        );
        assert_eq!(hash_decision(&r).len(), 64);
        assert!(hash_decision(&r).chars().all(|c| c.is_ascii_hexdigit()));
        // A flipped decision yields a different digest.
        let mut denied = sample_record();
        denied.decision = "deny".into();
        assert_ne!(hash_decision(&r), hash_decision(&denied));
        // A different argument hash is also reflected.
        let mut other_args = sample_record();
        other_args.args_hash = "ffffff".into();
        assert_ne!(hash_decision(&r), hash_decision(&other_args));
    }

    #[test]
    fn hash_args_is_stable_key_order_independent_and_distinguishes_content() {
        let a = json!({"path": "src/lib.rs", "mode": "write"});
        assert_eq!(hash_args(&a), hash_args(&a), "same args -> same hash");
        assert_eq!(hash_args(&a).len(), 64);
        // serde_json emits object keys in sorted order, so insertion order does not
        // perturb the digest.
        let reordered = json!({"mode": "write", "path": "src/lib.rs"});
        assert_eq!(hash_args(&a), hash_args(&reordered));
        // Different argument content yields a different digest.
        let b = json!({"path": "src/main.rs", "mode": "write"});
        assert_ne!(hash_args(&a), hash_args(&b));
    }
}
