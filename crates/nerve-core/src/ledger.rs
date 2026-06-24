//! Pure, golden-tested content-addressing for the L1 cross-run **transparency log**
//! (`docs/designs/trust-substrate.md` §3 L1, INV-R2). Where [`crate::provenance`]
//! seals the per-run event tape, this module folds a sequence of cross-run evidence
//! records ([`nerve_proto::ledger::LedgerRecord`]) into a linear, tamper-evident hash
//! chain: each record carries an identity digest over its `(seq, kind)`, a chained
//! `record_hash = sha256(prev_hash || identity)`, and the running [`LedgerHead`]
//! commits to the whole log.
//!
//! **This is the determinism boundary's L1 brick:** every function here is a pure
//! function of its arguments — no IO, no wall-clock, no randomness — so the same
//! sequence of appends yields a byte-identical chain, and a verifier can re-derive
//! (and thus detect tampering of) the spine from the records alone. `appended_at_ms`
//! is host metadata carried for display and is **never** hashed.
//!
//! SHA-256 (via `sha2`) is used for the same reason as L0: the chain underwrites a
//! portable audit trail that signed receipts (L4) will bind to. Persistence (which
//! DOES touch the world) lives above the kernel in `nerve-workstation` (INV-R2).

// Re-export the shapes this module content-addresses so a consumer of the kernel
// appends to (and verifies) the transparency log through `nerve_core` alone.
pub use nerve_proto::ledger::{
    AdvisoryJudge, LEDGER_SCHEMA_VERSION, LedgerHead, LedgerKind, LedgerRecord,
    PolicyDecisionOutcome,
};
use sha2::{Digest, Sha256};

/// Lowercase-hex SHA-256 of a record's **identity** — its position (`seq`) plus its
/// canonical-JSON payload (`kind`). This is the per-record digest that gets folded
/// into the chain; it deliberately excludes the chained `record_hash`/`prev_hash`
/// (which are derived from it) and the host-supplied `appended_at_ms` (not hashed),
/// so the identity of a record is a pure function of what it asserts and where.
#[must_use]
pub fn hash_record_identity(seq: u64, kind: &LedgerKind) -> String {
    let kind_bytes = serde_json::to_vec(kind).expect("LedgerKind serializes infallibly");
    let mut hasher = Sha256::new();
    hasher.update(seq.to_le_bytes());
    hasher.update(&kind_bytes);
    hex(hasher.finalize().as_slice())
}

/// Append one evidence record to the log, chaining it onto `head`. The new record's
/// `seq` is `head.count`, its `prev_hash` is the current `head.head_hash`, and its
/// `record_hash = sha256(prev_hash || hash_record_identity(seq, kind))`. Returns the
/// sealed [`LedgerRecord`] together with the advanced [`LedgerHead`] (count + 1, with
/// `head_hash` set to the new record's hash). Pure: `appended_at_ms` is carried onto
/// the record verbatim and is never hashed.
#[must_use]
pub fn append_record(
    head: &LedgerHead,
    kind: LedgerKind,
    appended_at_ms: u64,
) -> (LedgerRecord, LedgerHead) {
    let seq = head.count;
    let prev_hash = head.head_hash.clone();
    let identity = hash_record_identity(seq, &kind);
    let record_hash = chain(&prev_hash, &identity);
    let record = LedgerRecord {
        schema_version: LEDGER_SCHEMA_VERSION,
        seq,
        kind,
        record_hash: record_hash.clone(),
        prev_hash,
        appended_at_ms,
    };
    let next_head = LedgerHead {
        schema_version: LEDGER_SCHEMA_VERSION,
        count: head.count + 1,
        head_hash: record_hash,
    };
    (record, next_head)
}

/// Verify a full slice of records re-derives a consistent chain, returning the
/// [`LedgerHead`] that commits to it (the empty head for an empty slice). Three
/// independent failures are detected: a record whose recomputed `record_hash`
/// disagrees with the stored one ([`LedgerVerifyError::HashMismatch`]), a `seq` that
/// does not match its zero-based position ([`LedgerVerifyError::SeqGap`]), and a
/// `prev_hash` that does not equal the prior record's `record_hash`
/// ([`LedgerVerifyError::PrevMismatch`]). All checks are pure functions of the slice.
pub fn verify_chain(records: &[LedgerRecord]) -> Result<LedgerHead, LedgerVerifyError> {
    let mut prev_hash = String::new();
    for (index, record) in records.iter().enumerate() {
        let expected_seq = index as u64;
        if record.seq != expected_seq {
            return Err(LedgerVerifyError::SeqGap {
                expected: expected_seq,
                found: record.seq,
            });
        }
        if record.prev_hash != prev_hash {
            return Err(LedgerVerifyError::PrevMismatch { seq: record.seq });
        }
        let identity = hash_record_identity(record.seq, &record.kind);
        let recomputed = chain(&prev_hash, &identity);
        if recomputed != record.record_hash {
            return Err(LedgerVerifyError::HashMismatch { seq: record.seq });
        }
        prev_hash = record.record_hash.clone();
    }
    Ok(LedgerHead {
        schema_version: LEDGER_SCHEMA_VERSION,
        count: records.len() as u64,
        head_hash: prev_hash,
    })
}

/// The genesis head of an empty log: `count = 0`, empty `head_hash`. The first
/// [`append_record`] chains onto this (its `prev_hash` is therefore `""`).
#[must_use]
pub fn empty_head() -> LedgerHead {
    LedgerHead {
        schema_version: LEDGER_SCHEMA_VERSION,
        count: 0,
        head_hash: String::new(),
    }
}

/// Why a [`verify_chain`] re-derivation rejected a record slice. Each variant names
/// the `seq` (or the expected/found pair) of the offending record so a caller can
/// point a human at exactly where the transparency log diverged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerVerifyError {
    /// The record's recomputed `record_hash` did not equal its stored value — the
    /// payload or chaining was tampered with at this `seq`.
    HashMismatch {
        /// Sequence number of the record whose hash failed to reproduce.
        seq: u64,
    },
    /// A record's `seq` did not match its zero-based position in the slice — a record
    /// was inserted, dropped, or reordered.
    SeqGap {
        /// Sequence number the slice position required.
        expected: u64,
        /// Sequence number actually stored on the record.
        found: u64,
    },
    /// A record's `prev_hash` did not equal the prior record's `record_hash` — the
    /// chain was broken at this `seq`.
    PrevMismatch {
        /// Sequence number of the record whose back-link was wrong.
        seq: u64,
    },
}

impl std::fmt::Display for LedgerVerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HashMismatch { seq } => write!(f, "ledger record hash mismatch at seq {seq}"),
            Self::SeqGap { expected, found } => {
                write!(f, "ledger seq gap: expected {expected}, found {found}")
            }
            Self::PrevMismatch { seq } => write!(f, "ledger prev_hash mismatch at seq {seq}"),
        }
    }
}

impl std::error::Error for LedgerVerifyError {}

/// Fold one identity digest onto the running chain: `sha256(prev_hash || identity)`,
/// lowercase-hex. `prev_hash == ""` for the genesis link.
fn chain(prev_hash: &str, identity: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(identity.as_bytes());
    hex(hasher.finalize().as_slice())
}

/// Lowercase-hex encode bytes (mirrors [`crate::provenance`]'s encoder).
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

    fn run_recorded(n: u64) -> LedgerKind {
        LedgerKind::RunRecorded {
            run_id: format!("run-{n}"),
            run_root_hash: format!("root-{n}"),
            agent: "codex".into(),
            task_hash: format!("task-{n}"),
            event_count: n,
        }
    }

    /// Chain a fresh log of `n` `RunRecorded` records from the empty head.
    fn build_chain(n: u64) -> (Vec<LedgerRecord>, LedgerHead) {
        let mut head = empty_head();
        let mut records = Vec::new();
        for i in 0..n {
            let (record, next) = append_record(&head, run_recorded(i), 1000 + i);
            records.push(record);
            head = next;
        }
        (records, head)
    }

    #[test]
    fn identity_is_stable_and_distinguishes_content() {
        let kind = run_recorded(0);
        assert_eq!(
            hash_record_identity(0, &kind),
            hash_record_identity(0, &kind),
            "same (seq,kind) -> same identity"
        );
        // 64 lowercase hex chars (SHA-256).
        let id = hash_record_identity(0, &kind);
        assert_eq!(id.len(), 64);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        // A different seq with the same kind differs (seq is hashed).
        assert_ne!(
            hash_record_identity(0, &kind),
            hash_record_identity(1, &kind)
        );
        // A different payload at the same seq differs.
        assert_ne!(
            hash_record_identity(0, &kind),
            hash_record_identity(0, &run_recorded(7))
        );
    }

    #[test]
    fn empty_head_is_genesis() {
        let head = empty_head();
        assert_eq!(head.count, 0);
        assert_eq!(head.head_hash, "");
        assert_eq!(head.schema_version, LEDGER_SCHEMA_VERSION);
        // An empty slice verifies to the empty head.
        assert_eq!(verify_chain(&[]).unwrap(), head);
    }

    #[test]
    fn append_chains_and_advances_head() {
        let head0 = empty_head();
        let (r0, head1) = append_record(&head0, run_recorded(0), 1000);
        // First record links to genesis (empty prev).
        assert_eq!(r0.seq, 0);
        assert_eq!(r0.prev_hash, "");
        assert_eq!(head1.count, 1);
        assert_eq!(head1.head_hash, r0.record_hash);
        // Second record links to the first.
        let (r1, head2) = append_record(&head1, run_recorded(1), 1001);
        assert_eq!(r1.seq, 1);
        assert_eq!(r1.prev_hash, r0.record_hash);
        assert_eq!(head2.count, 2);
        assert_eq!(head2.head_hash, r1.record_hash);
        // Distinct records get distinct hashes.
        assert_ne!(r0.record_hash, r1.record_hash);
    }

    #[test]
    fn append_is_deterministic_and_wallclock_free() {
        let (_, head_a) = append_record(&empty_head(), run_recorded(0), 1000);
        // Same head + kind, DIFFERENT appended_at_ms -> identical chained hash
        // (the timestamp is carried but never hashed).
        let (_, head_b) = append_record(&empty_head(), run_recorded(0), 9_999_999);
        assert_eq!(head_a.head_hash, head_b.head_hash);
    }

    #[test]
    fn verify_chain_accepts_a_well_formed_log() {
        let (records, head) = build_chain(5);
        assert_eq!(verify_chain(&records).unwrap(), head);
    }

    #[test]
    fn verify_chain_detects_payload_tampering() {
        let (mut records, _) = build_chain(3);
        // Tamper the middle record's payload without rehashing.
        records[1].kind = run_recorded(99);
        assert_eq!(
            verify_chain(&records),
            Err(LedgerVerifyError::HashMismatch { seq: 1 })
        );
    }

    #[test]
    fn verify_chain_detects_seq_gap() {
        let (mut records, _) = build_chain(3);
        records[2].seq = 7;
        assert_eq!(
            verify_chain(&records),
            Err(LedgerVerifyError::SeqGap {
                expected: 2,
                found: 7
            })
        );
    }

    #[test]
    fn verify_chain_detects_broken_backlink() {
        let (mut records, _) = build_chain(3);
        // Break the back-link of the second record (seq stays correct so SeqGap
        // does not fire first).
        records[1].prev_hash = "deadbeef".into();
        assert_eq!(
            verify_chain(&records),
            Err(LedgerVerifyError::PrevMismatch { seq: 1 })
        );
    }
}
