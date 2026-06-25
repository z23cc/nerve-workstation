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

use crate::ledger_store::{LedgerStore, run_ledger_query, verify_error_class};
use anyhow::{Context, Result};
use clap::Args;
use nerve_core::ledger::verify_chain;
use serde_json::{Value, json};
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

/// `nerve ledger query` — read the append-only evidence ledger offline (read-only),
/// symmetric with `nerve ledger verify`. Narrows by the same optional facets the
/// `ledger.query` protocol command carries (incl. the v13 `run_root_hash` lineage facet)
/// and prints the matching records (a human table or, with `--json`, a JSON array).
#[derive(Debug, Args)]
pub(crate) struct LedgerQueryArgs {
    /// Workspace root holding `.nerve/` (defaults to the current directory).
    #[arg(long = "root")]
    root: Option<PathBuf>,
    /// Filter to records about this run id.
    #[arg(long = "run-id")]
    run_id: Option<String>,
    /// Filter to a run's whole lineage by its content address (v13).
    #[arg(long = "run-root-hash")]
    run_root_hash: Option<String>,
    /// Filter by ledger record kind (`run_recorded`, `verdict`, `receipt_issued`, …).
    #[arg(long = "record-kind")]
    record_kind: Option<String>,
    /// Filter by the agent named on a `RunRecorded` record.
    #[arg(long = "agent")]
    agent: Option<String>,
    /// Cap the number of (newest-first) records returned.
    #[arg(long = "limit")]
    limit: Option<u64>,
    /// Emit the machine-readable JSON array instead of the human table.
    #[arg(long = "json")]
    json: bool,
}

/// Read the ledger for `args.root`, filter by the supplied facets, and print the matching
/// records. Read-only and daemon-free (`LedgerStore::for_scope` → `run_ledger_query`);
/// always exits `0` (a query that matches nothing is still a successful read).
pub(crate) fn query(args: LedgerQueryArgs) -> Result<i32> {
    let root = resolve_root(args.root)?;
    let store = LedgerStore::for_scope(Some(&root)).context("open ledger store")?;
    let result = run_ledger_query(
        Some(&store),
        args.run_id.as_deref(),
        args.agent.as_deref(),
        None,
        args.run_root_hash.as_deref(),
        None,
        args.record_kind.as_deref(),
        args.limit.unwrap_or(200),
    );
    let records = result
        .get("records")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if args.json {
        println!("{}", Value::Array(records));
    } else {
        print_records_table(&records);
    }
    Ok(0)
}

/// Print one line per record (`seq  kind  run_id  run_root_hash`), newest first. The seq
/// + kind + lineage pointers are the audit-relevant columns; `--json` carries the rest.
fn print_records_table(records: &[Value]) {
    if records.is_empty() {
        println!("no ledger records match");
        return;
    }
    for record in records {
        let seq = record.get("seq").and_then(Value::as_u64).unwrap_or(0);
        let kind = record
            .pointer("/kind/kind")
            .and_then(Value::as_str)
            .unwrap_or("?");
        let run_id = record
            .pointer("/kind/run_id")
            .and_then(Value::as_str)
            .unwrap_or("-");
        let root_hash = record
            .pointer("/kind/run_root_hash")
            .and_then(Value::as_str)
            .unwrap_or("-");
        println!("{seq}\t{kind}\t{run_id}\t{root_hash}");
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

    fn query_args(root: &std::path::Path) -> LedgerQueryArgs {
        LedgerQueryArgs {
            root: Some(root.to_path_buf()),
            run_id: None,
            run_root_hash: None,
            record_kind: None,
            agent: None,
            limit: None,
            json: false,
        }
    }

    #[test]
    fn query_reads_records_and_exits_zero() {
        let dir = tempdir().unwrap();
        let store = LedgerStore::for_scope(Some(dir.path())).unwrap();
        store.append(run_recorded(0)).unwrap();
        store.append(run_recorded(1)).unwrap();

        // Human table over a populated ledger exits 0.
        let code = query(query_args(dir.path())).unwrap();
        assert_eq!(code, 0);

        // A filter that matches nothing is still a successful read (exit 0).
        let mut miss = query_args(dir.path());
        miss.run_id = Some("does-not-exist".into());
        assert_eq!(query(miss).unwrap(), 0);

        // An empty (never-written) ledger reads cleanly.
        let empty = tempdir().unwrap();
        assert_eq!(query(query_args(empty.path())).unwrap(), 0);
    }

    #[test]
    fn query_by_run_root_hash_selects_lineage_and_json_shape() {
        use nerve_core::verdict::VerdictStatus;

        let dir = tempdir().unwrap();
        let store = LedgerStore::for_scope(Some(dir.path())).unwrap();
        // RunRecorded(root-0) + a Verdict pinned to it, then an unrelated run.
        store.append(run_recorded(0)).unwrap(); // run_root_hash "root-0"
        store
            .append(LedgerKind::Verdict {
                run_id: "run-0".into(),
                diff_hash: None,
                verdict: VerdictStatus::Passed,
                checks: vec![],
                advisory_llm_judge: None,
                run_root_hash: Some("root-0".into()),
            })
            .unwrap();
        store.append(run_recorded(1)).unwrap(); // run_root_hash "root-1"

        // --json over the lineage filter: a JSON array of exactly the two root-0 records.
        let store2 = LedgerStore::for_scope(Some(dir.path())).unwrap();
        let result = run_ledger_query(
            Some(&store2),
            None,
            None,
            None,
            Some("root-0"),
            None,
            None,
            200,
        );
        let records = result["records"].as_array().unwrap();
        assert_eq!(records.len(), 2, "RunRecorded + Verdict for root-0");
        for rec in records {
            assert_eq!(rec["kind"]["run_root_hash"], json!("root-0"));
        }

        // The CLI path with the same facet exits 0 (--json on).
        let mut args = query_args(dir.path());
        args.run_root_hash = Some("root-0".into());
        args.json = true;
        assert_eq!(query(args).unwrap(), 0);
    }
}
