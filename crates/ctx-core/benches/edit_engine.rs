//! Micro-benchmarks for the four edit modes over a synthetic source file.
//!
//! These exercise the planning hot path (`edit::apply`) for each mode; they are
//! pure in-memory transforms (no filesystem), so they isolate parser/applier
//! cost. Run with `cargo bench -p ctx-core --bench edit_engine`.

use criterion::{Criterion, criterion_group, criterion_main};
use ctx_core::edit::{self, EditRequest, PatchEntry, ReplaceEdit, snapshot_tag};
use std::collections::BTreeMap;
use std::hint::black_box;

const PATH: &str = "src/bench.rs";
const LINES: usize = 600;

/// A deterministic multi-line Rust-ish source file.
fn source() -> String {
    let mut out = String::with_capacity(LINES * 24);
    for idx in 0..LINES {
        out.push_str(&format!("    let value_{idx} = compute_{idx}(input);\n"));
    }
    out
}

fn reader(content: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert(PATH.to_string(), content.to_string());
    map
}

fn bench_edit_modes(c: &mut Criterion) {
    let content = source();
    let reader = reader(&content);
    let target = "    let value_300 = compute_300(input);";

    let mut group = c.benchmark_group("edit_apply");

    group.bench_function("replace", |b| {
        let req = EditRequest::Replace {
            path: PATH.to_string(),
            edits: vec![ReplaceEdit {
                old_text: target.to_string(),
                new_text: "    let value_300 = recompute_300(input);".to_string(),
                all: false,
            }],
        };
        b.iter(|| edit::apply(black_box(&req), black_box(&reader)).expect("replace"));
    });

    group.bench_function("patch", |b| {
        let diff = format!("@@ value_300\n-{target}\n+    let value_300 = recompute_300(input);");
        let req = EditRequest::Patch {
            path: PATH.to_string(),
            entries: vec![PatchEntry::Update { rename: None, diff }],
        };
        b.iter(|| edit::apply(black_box(&req), black_box(&reader)).expect("patch"));
    });

    group.bench_function("apply_patch", |b| {
        let patch = format!(
            "*** Begin Patch\n*** Update File: {PATH}\n@@ value_300\n-{target}\n+    let value_300 = recompute_300(input);\n*** End Patch\n"
        );
        let req = EditRequest::ApplyPatch { patch };
        b.iter(|| edit::apply(black_box(&req), black_box(&reader)).expect("apply_patch"));
    });

    group.bench_function("hashline", |b| {
        let tag = snapshot_tag(&content);
        let patch = format!(
            "*** Begin Patch\n[{PATH}#{tag}]\nSWAP 301.=301:\n+    let value_300 = recompute_300(input);\n*** End Patch\n"
        );
        let req = EditRequest::Hashline { patch };
        b.iter(|| edit::apply(black_box(&req), black_box(&reader)).expect("hashline"));
    });

    group.finish();
}

criterion_group!(benches, bench_edit_modes);
criterion_main!(benches);
