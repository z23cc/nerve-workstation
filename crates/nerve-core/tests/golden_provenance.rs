//! Golden lock on the L0 run-capture canonicalization + content-addressing
//! (`docs/designs/trust-substrate.md` §3/§5, INV-R2). The input tape is hardcoded
//! with fixed logical `seq`s and fixed host timestamps — no wall-clock, no
//! randomness — so the snapshotted `root_hash` / `ledger` / `events` are a pure
//! function of the algorithm. Any change that perturbs the digest (a serde-version
//! bump, a field reorder, a canonicalization change) flips this snapshot and fails
//! CI: the content address that portable receipts (L4) will sign cannot drift
//! silently.

use nerve_core::provenance::{Event, EventKind, build_run};
use serde_json::json;

fn ev(seq: u64, kind: EventKind) -> Event {
    Event { seq, kind }
}

#[test]
fn golden_run_hash() {
    let events = vec![
        ev(
            0,
            EventKind::RunStarted {
                agent: "codex".into(),
                task: "add a regression test for the parser".into(),
                cwd: Some("/repo".into()),
                // None -> skip_serialized, so the locked content address is unchanged
                // by the L0c additive field (RISK #8 regression lock).
                inputs: None,
            },
        ),
        ev(1, EventKind::TurnStarted { turn: 0 }),
        ev(
            2,
            EventKind::Output {
                turn: 0,
                text: "reading src/parser.rs".into(),
            },
        ),
        ev(
            3,
            EventKind::Output {
                turn: 0,
                text: "applied edit to tests/parser.rs".into(),
            },
        ),
        ev(
            4,
            EventKind::UsageUpdated {
                turn: 0,
                input_tokens: 1200,
                output_tokens: 340,
                cache_read_tokens: 800,
                cache_creation_tokens: 0,
                cost_micro_usd: Some(4200),
            },
        ),
        ev(5, EventKind::TurnFinished { turn: 0, ok: true }),
        ev(
            6,
            EventKind::RunFinished {
                ok: true,
                exit_code: Some(0),
                timed_out: false,
            },
        ),
    ];

    let run = build_run(
        "delegate-session-fixed",
        "codex",
        Some("/repo".into()),
        1_000,
        Some(2_000),
        true,
        events,
        nerve_core::provenance::RunInputs::default(),
    );

    // run_id IS the content address of the tape (== root_hash) at L0.
    assert_eq!(run.run_id, run.root_hash);

    insta::assert_json_snapshot!(json!({
        "run_id": run.run_id,
        "root_hash": run.root_hash,
        "ledger": run.ledger,
        "events": run.events,
    }));
}
