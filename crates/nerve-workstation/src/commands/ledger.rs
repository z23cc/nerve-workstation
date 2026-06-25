//! The L1 **CLI ledger re-verify flow** (`docs/designs/trust-substrate.md` §3 L1,
//! INV-R5) — the offline, daemon-free half of `nerve ledger verify`. A third party or
//! CI can demand re-derivation of the append-only evidence ledger without standing up a
//! daemon: this reads `<root>/.nerve/ledger/log.ndjson`, re-derives the hash chain via
//! the pure [`nerve_core::ledger::verify_chain`], and reports whether it is intact.
//!
//! **Court reporter, not judge (INV-R1).** This asserts nothing about correctness — only
//! that the transparency log is append-only and untampered. An intact chain exits 0
//! (`ledger intact: N records, head <hash>`); the first divergence exits non-zero,
//! naming the tamper class (`HashMismatch` / `SeqGap` / `PrevMismatch`) and the `seq`
//! where the re-derivation broke. The chaining/hashing it calls is pure in `nerve-core`
//! (INV-R2); only the read + print live here above the determinism boundary.

use crate::ledger_store::{LedgerStore, verify_error_class};
use anyhow::{Context, Result};
use clap::Args;
use nerve_core::ledger::verify_chain;
use serde_json::json;
use std::path::PathBuf;

/// `nerve ledger verify` — re-derive the cross-run evidence ledger and report whether
/// its hash chain is intact. Read-only; the exit code IS the result (0 intact, non-zero
/// tampered/unreadable), so CI can gate on it.
#[derive(Debug, Args)]
pub(crate) struct LedgerVerifyArgs {
    /// Workspace root holding `.nerve/` (defaults to the current directory).
    #[arg(long = "root")]
    root: Option<PathBuf>,
    /// Emit the machine-readable JSON result instead of the human line.
    #[arg(long = "json")]
    json: bool,
}

/// Re-derive the ledger for `args.root` and report. Returns the process exit code: `0`
/// on an intact chain, `1` on a detected tamper (the re-derivation diverged). An
/// unreadable store is surfaced as an `Err` (a real IO failure, not a tamper verdict).
pub(crate) fn verify(args: LedgerVerifyArgs) -> Result<i32> {
    let root = resolve_root(args.root)?;
    let store = LedgerStore::for_scope(Some(&root)).context("open ledger store")?;
    let records = store.read_all();
    match verify_chain(&records) {
        Ok(head) => {
            report_intact(head.count, &head.head_hash, args.json);
            Ok(0)
        }
        Err(err) => {
            let (class, seq) = verify_error_class(&err);
            report_tamper(class, seq, args.json);
            Ok(1)
        }
    }
}

/// Print the intact verdict (`ledger intact: N records, head <hash>`), or its JSON form.
fn report_intact(count: u64, head_hash: &str, as_json: bool) {
    if as_json {
        println!(
            "{}",
            json!({ "ok": true, "count": count, "head_hash": head_hash })
        );
    } else {
        let head = if head_hash.is_empty() { "-" } else { head_hash };
        println!("ledger intact: {count} records, head {head}");
    }
}

/// Print the tamper verdict (the class + the diverging `seq`), or its JSON form.
fn report_tamper(class: &str, seq: u64, as_json: bool) {
    if as_json {
        println!("{}", json!({ "ok": false, "error": class, "seq": seq }));
    } else {
        eprintln!("ledger TAMPERED: {class} at seq {seq}");
    }
}

/// Resolve the workspace root (defaults to the current directory; mirrors `gate.rs`).
fn resolve_root(root: Option<PathBuf>) -> Result<PathBuf> {
    match root {
        Some(root) => Ok(root),
        None => std::env::current_dir().context("failed to resolve current directory"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nerve_core::ledger::LedgerKind;
    use tempfile::tempdir;

    fn run_recorded(n: u64) -> LedgerKind {
        LedgerKind::RunRecorded {
            run_id: format!("run-{n}"),
            run_root_hash: format!("root-{n}"),
            agent: "codex".into(),
            task_hash: format!("task-{n}"),
            event_count: n,
        }
    }

    #[test]
    fn intact_ledger_exits_zero() {
        let dir = tempdir().unwrap();
        let store = LedgerStore::for_scope(Some(dir.path())).unwrap();
        store.append(run_recorded(0)).unwrap();
        store.append(run_recorded(1)).unwrap();

        let code = verify(LedgerVerifyArgs {
            root: Some(dir.path().to_path_buf()),
            json: false,
        })
        .unwrap();
        assert_eq!(code, 0);
    }

    #[test]
    fn empty_ledger_is_intact() {
        let dir = tempdir().unwrap();
        let code = verify(LedgerVerifyArgs {
            root: Some(dir.path().to_path_buf()),
            json: true,
        })
        .unwrap();
        assert_eq!(code, 0);
    }

    #[test]
    fn tampered_ledger_exits_nonzero() {
        let dir = tempdir().unwrap();
        let store = LedgerStore::for_scope(Some(dir.path())).unwrap();
        let r0 = store.append(run_recorded(0)).unwrap();
        store.append(run_recorded(1)).unwrap();

        // Flip a byte in the first record's payload without rehashing: the log line is
        // still valid JSON (so `read_all` keeps it) but `verify_chain` rejects it.
        let log = store.dir().join("log.ndjson");
        let raw = std::fs::read_to_string(&log).unwrap();
        let tampered = raw.replacen("run-0", "run-X", 1);
        assert_ne!(raw, tampered, "tamper actually changed a byte");
        std::fs::write(&log, tampered).unwrap();
        // The record hash on disk is still r0's, so the recompute diverges.
        assert!(!r0.record_hash.is_empty());

        let code = verify(LedgerVerifyArgs {
            root: Some(dir.path().to_path_buf()),
            json: false,
        })
        .unwrap();
        assert_eq!(code, 1);
    }
}
