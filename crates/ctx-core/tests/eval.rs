//! Repo-level retrieval eval harness (baseline / measuring stick).
//!
//! Runs a hand-labeled query set (`tests/fixtures/eval/queries.json`) against the
//! real hybrid `semantic_search` pipeline over THIS repository and reports
//! recall@k / MRR / symbol-hit. Run it before and after any model or pipeline
//! change to see whether the change actually helps your own queries — public
//! benchmarks (MTEB-Code, etc.) do not measure your repo or your query mix.
//!
//! Ignored by default: it needs the local ONNX embedding model and builds the
//! dense index over the whole repo (~tens of seconds cold). Run on demand:
//!
//!   cargo test -p ctx-core --features semantic --test eval -- --ignored --nocapture
//!
//! Knobs (env):
//!   EVAL_K=10        top-k cutoff for recall@k / hit / symbol-hit (default 10)
//!   EVAL_RERANK=1    also construct + apply the reranker (default off: the
//!                    pre-rerank fused candidate recall is the real bottleneck
//!                    signal — rerank only reorders what was already retrieved).
//!
//! The assertion floor is intentionally a low sanity check. Once you trust the
//! printed baseline, raise it to lock in a regression gate.

#![cfg(all(feature = "semantic", not(target_arch = "wasm32")))]

use ctx_core::{
    CancelToken, CatalogProvider, FsCatalogProvider, RootPolicy, ScanOptions, SemanticSearchMode,
    SemanticSearchRequest, semantic::SemanticRuntimeConfig,
};
use serde::Deserialize;
use std::{path::PathBuf, time::Instant};

#[derive(Deserialize)]
struct EvalQuery {
    id: String,
    query: String,
    #[serde(default)]
    expected_files: Vec<String>,
    #[serde(default)]
    expected_symbols: Vec<String>,
}

#[derive(Deserialize)]
struct EvalSet {
    queries: Vec<EvalQuery>,
}

/// `crates/ctx-core` -> repository root.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root from CARGO_MANIFEST_DIR")
        .to_path_buf()
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_flag(key: &str) -> bool {
    matches!(
        std::env::var(key).ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        value.to_string()
    } else {
        format!("{}…", &value[..max - 1])
    }
}

#[test]
#[ignore = "needs local ONNX model + builds the dense index over the repo; run with --ignored --nocapture"]
fn semantic_search_recall_baseline() {
    let root = repo_root();
    let k = env_usize("EVAL_K", 10);
    let rerank = env_flag("EVAL_RERANK");

    let policy = RootPolicy::new(vec![root.clone()]).expect("root policy");
    let provider = FsCatalogProvider::new(
        RootPolicy::new(vec![root.clone()]).expect("provider policy"),
        ScanOptions::default(),
    );
    let snapshot = provider.snapshot().expect("snapshot");

    let config = SemanticRuntimeConfig {
        enabled: true,
        rerank,
        ..SemanticRuntimeConfig::disabled()
    };
    let index = config
        .build_index_for_roots(policy.roots())
        .expect("build semantic index")
        .expect("semantic index enabled");

    let set: EvalSet = serde_json::from_str(include_str!("fixtures/eval/queries.json"))
        .expect("parse eval dataset");
    let n = set.queries.len();
    assert!(n > 0, "empty eval dataset");

    let mut file_hits = 0usize;
    let mut mrr_sum = 0.0f64;
    let mut symbol_total = 0usize;
    let mut symbol_hits = 0usize;

    println!();
    println!("repo-level retrieval eval   (k={k}, rerank={rerank}, n={n})");
    println!(
        "{:<18} {:>5} {:>5} {:>4}  {}",
        "query", "hit", "rank", "sym", "top-1 path"
    );
    println!("{}", "-".repeat(96));

    let started = Instant::now();
    for q in &set.queries {
        let response = index
            .search(
                &provider,
                &snapshot,
                &SemanticSearchRequest {
                    query: q.query.clone(),
                    max_results: k,
                    mode: SemanticSearchMode::Hybrid,
                    rerank,
                    ..SemanticSearchRequest::default()
                },
                &CancelToken::never(),
            )
            .expect("semantic search");

        let rank = response
            .results
            .iter()
            .take(k)
            .position(|r| q.expected_files.iter().any(|e| e == &r.path))
            .map(|i| i + 1);
        if let Some(r) = rank {
            file_hits += 1;
            mrr_sum += 1.0 / r as f64;
        }

        let symbol_hit = if q.expected_symbols.is_empty() {
            None
        } else {
            symbol_total += 1;
            let hit = response.results.iter().take(k).any(|r| {
                r.symbol
                    .as_deref()
                    .is_some_and(|s| q.expected_symbols.iter().any(|e| e == s))
            });
            if hit {
                symbol_hits += 1;
            }
            Some(hit)
        };

        println!(
            "{:<18} {:>5} {:>5} {:>4}  {}",
            truncate(&q.id, 18),
            if rank.is_some() { "yes" } else { "NO" },
            rank.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
            match symbol_hit {
                None => "-",
                Some(true) => "yes",
                Some(false) => "no",
            },
            response.results.first().map(|r| r.path.as_str()).unwrap_or("-"),
        );
    }
    let elapsed = started.elapsed();

    let nf = n as f64;
    let symbol_rate = if symbol_total == 0 {
        f64::NAN
    } else {
        symbol_hits as f64 / symbol_total as f64
    };
    println!("{}", "-".repeat(96));
    println!(
        "recall@{k} (file): {:.3}    MRR: {:.3}    symbol-hit@{k}: {:.3} ({}/{})    {:.1}s total",
        file_hits as f64 / nf,
        mrr_sum / nf,
        symbol_rate,
        symbol_hits,
        symbol_total,
        elapsed.as_secs_f64(),
    );
    println!("(baseline run — raise the assertion floor in tests/eval.rs to lock a regression gate)\n");

    assert!(
        file_hits > 0,
        "no query retrieved an expected file in top-{k}: model load or pipeline is broken"
    );
}
