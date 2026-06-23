//! Process-global, snapshot-memoized derived reference graph.
//!
//! `get_repo_map` (and `build_context`, which calls it) used to rebuild the whole
//! cross-file [`ReferenceGraph`] — an O(edges) pass over every reference in the
//! repo — on **every** call, even when the snapshot and its shared
//! [`IndexedFile`] set were unchanged. This memoizes that derived graph **once per
//! snapshot** and reuses it while the provider serves the same cached `Arc`.
//!
//! ## Why this is byte-identical
//!
//! [`ReferenceGraph::build`] is a **pure** function of `&[IndexedFile]`: it reads
//! only `file.references`, `file.symbols`, and `language_family`, never
//! `query_match`. The shared index from [`shared_indexed_files`] is the exact
//! `Indexed`-filtered, path-sorted set `get_repo_map` would build itself, so a
//! graph built off it is identical to the per-call graph; caching a pure
//! function's output is byte-identical by construction.
//!
//! Keying, eviction, and the hit==miss guarantee live in [`super::snapshot_memo`].

use std::sync::{Arc, Mutex, OnceLock};

use crate::{
    cancel::CancelToken, models::NerveError, port::CatalogProvider, repomap::ReferenceGraph,
    snapshot::CatalogSnapshot,
};

use super::shared_indexed_files;
use super::snapshot_memo::SnapshotMemo;

/// Maximum number of distinct snapshots whose reference graph is retained at
/// once. Mirrors the sibling memos' caps.
const SHARED_GRAPH_CAP: usize = 8;

fn cache() -> &'static Mutex<SnapshotMemo<ReferenceGraph>> {
    static CACHE: OnceLock<Mutex<SnapshotMemo<ReferenceGraph>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(SnapshotMemo::new(SHARED_GRAPH_CAP)))
}

/// Return the memoized [`ReferenceGraph`] for `snapshot`, building it once on a
/// miss from the shared indexed-file set.
pub(crate) fn shared_reference_graph<P: CatalogProvider + Sync>(
    provider: &P,
    snapshot: &Arc<CatalogSnapshot>,
    cancel: &CancelToken,
) -> Result<Arc<ReferenceGraph>, NerveError> {
    SnapshotMemo::get_or_build(cache(), snapshot, || {
        let files = shared_indexed_files(provider, snapshot, cancel)?;
        ReferenceGraph::build_cancellable(&files, cancel)
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::catalog::{FsCatalogProvider, ScanOptions};
    use crate::security::RootPolicy;
    use std::fs;

    fn provider_for(dir: &std::path::Path) -> FsCatalogProvider {
        FsCatalogProvider::new(
            RootPolicy::new(vec![dir.to_path_buf()]).expect("policy"),
            ScanOptions::default(),
        )
    }

    #[test]
    fn same_cached_snapshot_returns_ptr_eq_reference_graph() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("a.rs"),
            "pub fn one() { two(); }\npub fn two() {}\n",
        )
        .expect("write");
        let provider = provider_for(dir.path());

        let snapshot = provider.snapshot_arc().expect("snapshot");
        let first =
            shared_reference_graph(&provider, &snapshot, &CancelToken::never()).expect("graph");
        let second =
            shared_reference_graph(&provider, &snapshot, &CancelToken::never()).expect("graph");

        // Hit path: identical snapshot Arc -> the very same memoized graph.
        assert!(
            Arc::ptr_eq(&first, &second),
            "repeated calls on the same snapshot Arc must reuse the memoized reference graph"
        );
    }

    #[test]
    fn fs_provider_edit_invalidate_serves_fresh_graph_not_stale_memo() {
        let dir = tempfile::tempdir().expect("tempdir");
        // `caller.rs` references `make_target` defined in `target.rs`: one
        // file->file edge (caller -> target).
        let target = dir.path().join("target.rs");
        let caller = dir.path().join("caller.rs");
        let extra = dir.path().join("extra.rs");
        fs::write(&target, "pub fn make_target() -> usize { 1 }\n").expect("write target");
        fs::write(&caller, "pub fn caller() -> usize { make_target() }\n").expect("write caller");
        let provider = provider_for(dir.path());

        // First snapshot + reference graph reflects the original edge set.
        let snapshot_a = provider.snapshot_arc().expect("snapshot a");
        let graph_a =
            shared_reference_graph(&provider, &snapshot_a, &CancelToken::never()).expect("graph a");
        let edges_a = graph_a.edge_count;
        let symbols_a = graph_a.symbols_indexed;
        assert!(edges_a >= 1, "expected at least the caller->target edge");

        // Add a brand-new defining file (`extra.rs`) and have `caller` call into
        // it, so a genuinely new file->file edge appears and the indexed-symbol
        // count grows. A graph rebuilt off the new snapshot must reflect both; a
        // stale memo would report the old counts.
        fs::write(&extra, "pub fn other() -> usize { 2 }\n").expect("write extra");
        fs::write(
            &caller,
            "pub fn caller() -> usize { make_target() + other() }\n",
        )
        .expect("rewrite caller");
        provider.invalidate();

        // Second snapshot is a brand-new Arc (not ptr_eq to snapshot_a), so the
        // memo must miss and rebuild — never serve the stale graph (hit == miss).
        let snapshot_b = provider.snapshot_arc().expect("snapshot b");
        assert!(
            !Arc::ptr_eq(&snapshot_a, &snapshot_b),
            "invalidate must force a fresh snapshot Arc after an edit"
        );
        let graph_b =
            shared_reference_graph(&provider, &snapshot_b, &CancelToken::never()).expect("graph b");

        assert!(
            graph_b.symbols_indexed > symbols_a,
            "reference graph after edit+invalidate must index the new symbol, not the stale memo \
             (was {symbols_a}, now {})",
            graph_b.symbols_indexed
        );
        assert!(
            graph_b.edge_count > edges_a,
            "reference graph after edit+invalidate must reflect the new edge, not the stale memo \
             (was {edges_a}, now {})",
            graph_b.edge_count
        );
    }
}
