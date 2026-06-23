//! Process-global, snapshot-memoized shared indexed-file set (CodeGraph T0).
//!
//! Every navigation / `build_context` call used to re-run
//! [`indexed_files_cancellable`] from scratch — re-reading bytes, re-collecting
//! symbols, and re-sorting a fresh `Vec<IndexedFile>` even though the underlying
//! parses are already codemap-cached by the provider. This builds that
//! `Vec<IndexedFile>` **once per snapshot** and reuses it across all callers for
//! as long as the provider keeps serving the same cached snapshot `Arc`.
//!
//! Keying, eviction, and the hit==miss determinism guarantee live in
//! [`super::snapshot_memo`]; this module is just the indexed-file instantiation.

use std::sync::{Arc, Mutex, OnceLock};

use crate::{
    cancel::CancelToken,
    models::NerveError,
    port::CatalogProvider,
    repomap::{IndexedFile, indexed_files_cancellable},
    snapshot::CatalogSnapshot,
};

use super::snapshot_memo::SnapshotMemo;

/// Maximum number of distinct snapshots whose shared index is retained at once.
/// Small on purpose: a daemon usually drives one or a few live snapshots.
const SHARED_INDEX_CAP: usize = 8;

fn cache() -> &'static Mutex<SnapshotMemo<Vec<IndexedFile>>> {
    static CACHE: OnceLock<Mutex<SnapshotMemo<Vec<IndexedFile>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(SnapshotMemo::new(SHARED_INDEX_CAP)))
}

/// Return the memoized indexed-file set for `snapshot`, building it once on a
/// miss via the existing [`indexed_files_cancellable`] logic. A hit is
/// byte-identical to a miss (same snapshot → same output).
pub(crate) fn shared_indexed_files<P: CatalogProvider + Sync>(
    provider: &P,
    snapshot: &Arc<CatalogSnapshot>,
    cancel: &CancelToken,
) -> Result<Arc<Vec<IndexedFile>>, NerveError> {
    SnapshotMemo::get_or_build(cache(), snapshot, || {
        indexed_files_cancellable(provider, snapshot, cancel)
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
    fn same_cached_snapshot_returns_ptr_eq_shared_index() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("lib.rs"), "pub fn alpha() {}\n").expect("write");
        let provider = provider_for(dir.path());

        let snapshot = provider.snapshot_arc().expect("snapshot");
        let first = shared_indexed_files(&provider, &snapshot, &CancelToken::never()).expect("idx");
        let second =
            shared_indexed_files(&provider, &snapshot, &CancelToken::never()).expect("idx");

        // Hit path: identical snapshot Arc -> the very same memoized vec.
        assert!(
            Arc::ptr_eq(&first, &second),
            "repeated calls on the same snapshot Arc must reuse the memoized index"
        );
    }

    #[test]
    fn fs_provider_edit_invalidate_serves_fresh_index_not_stale_memo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lib.rs");
        fs::write(&path, "pub fn alpha() {}\n").expect("write");
        let provider = provider_for(dir.path());

        // First snapshot + shared index reflects the original symbol set.
        let snapshot_a = provider.snapshot_arc().expect("snapshot a");
        let index_a =
            shared_indexed_files(&provider, &snapshot_a, &CancelToken::never()).expect("idx a");
        let names_a: Vec<&str> = index_a
            .iter()
            .flat_map(|file| file.symbols.iter())
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert!(names_a.contains(&"alpha"));
        assert!(!names_a.contains(&"beta"));

        // Edit the file and invalidate exactly as the provider's write path does,
        // dropping the cached snapshot Arc so the next call builds a fresh one.
        fs::write(&path, "pub fn alpha() {}\npub fn beta() {}\n").expect("rewrite");
        provider.invalidate();

        // Second snapshot is a brand-new Arc (not ptr_eq to snapshot_a) so the
        // memo must miss and rebuild — never serve the stale index (hit == miss).
        let snapshot_b = provider.snapshot_arc().expect("snapshot b");
        assert!(
            !Arc::ptr_eq(&snapshot_a, &snapshot_b),
            "invalidate must force a fresh snapshot Arc after an edit"
        );
        let index_b =
            shared_indexed_files(&provider, &snapshot_b, &CancelToken::never()).expect("idx b");
        let names_b: Vec<&str> = index_b
            .iter()
            .flat_map(|file| file.symbols.iter())
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert!(
            names_b.contains(&"beta"),
            "shared index after edit+invalidate must reflect the new symbol, not the stale memo"
        );
    }

    #[test]
    fn shared_index_matches_direct_indexed_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("a.rs"),
            "pub fn one() { two(); }\npub fn two() {}\n",
        )
        .expect("write a");
        fs::write(dir.path().join("b.rs"), "pub struct Widget;\n").expect("write b");
        let provider = provider_for(dir.path());
        let snapshot = provider.snapshot_arc().expect("snapshot");

        let shared =
            shared_indexed_files(&provider, &snapshot, &CancelToken::never()).expect("shared");
        let direct =
            indexed_files_cancellable(&provider, &snapshot, &CancelToken::never()).expect("direct");

        // Parity: the memoized vec is byte-identical to a fresh per-call build.
        assert_eq!(shared.len(), direct.len());
        for (memoized, fresh) in shared.iter().zip(direct.iter()) {
            assert_eq!(memoized.path, fresh.path);
            assert_eq!(memoized.symbols.len(), fresh.symbols.len());
            assert_eq!(memoized.references.len(), fresh.references.len());
        }
    }
}
