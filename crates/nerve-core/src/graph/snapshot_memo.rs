//! A bounded, process-global, snapshot-`Arc`-identity-keyed memo of a derived
//! value — the shared engine behind every CodeGraph T0 cache
//! (`shared_indexed_files`, `shared_reference_graph`, `shared_definition_index`).
//!
//! ## Memo key (the correctness crux)
//!
//! Keyed on **snapshot `Arc` identity** via a [`Weak`] reference confirmed with
//! [`Arc::ptr_eq`], never on `CatalogSnapshot.generation`. `FsCatalogProvider`
//! hard-codes `generation = 1` permanently (`catalog/fs_scan.rs`
//! `finalize_snapshot`); the real edit counter lives in a separate
//! `ProviderCache.generation` that never reaches the snapshot value tools observe,
//! so a generation-keyed memo would serve a **stale** value after an edit.
//!
//! The provider re-serves the *same* `Arc<CatalogSnapshot>` within its cache TTL
//! and drops it to `None` on every write/delete/rename (`invalidate()`), so the
//! next call after an edit builds a fresh `Arc` and the memo misses (hit == miss).
//! Only a [`Weak`] to the snapshot is stored, so the memo never pins a snapshot in
//! memory; an over-budget eviction drops only the derived value.

use std::sync::{Arc, Mutex, Weak};

use crate::{models::NerveError, snapshot::CatalogSnapshot};

/// MRU-ordered (front = most recently used), bounded list of
/// `(snapshot identity, derived value)` entries.
pub(super) struct SnapshotMemo<T> {
    entries: Vec<(Weak<CatalogSnapshot>, Arc<T>)>,
    cap: usize,
}

impl<T> SnapshotMemo<T> {
    /// A memo retaining at most `cap` distinct snapshots' values.
    pub(super) const fn new(cap: usize) -> Self {
        Self {
            entries: Vec::new(),
            cap,
        }
    }

    /// Return the memoized value for `snapshot`, building it once on a miss.
    ///
    /// On a hit the returned `Arc` is `Arc::ptr_eq`-equal across repeated calls
    /// that observe the same cached snapshot `Arc`. `build` runs **without**
    /// holding `memo`'s lock, so concurrent callers on different snapshots never
    /// serialize on an O(repo) build; a racing duplicate build is reconciled to a
    /// single canonical `Arc` on re-check.
    pub(super) fn get_or_build(
        memo: &Mutex<Self>,
        snapshot: &Arc<CatalogSnapshot>,
        build: impl FnOnce() -> Result<T, NerveError>,
    ) -> Result<Arc<T>, NerveError> {
        if let Some(value) = Self::lookup(memo, snapshot) {
            return Ok(value);
        }
        let built = Arc::new(build()?);
        Ok(Self::insert(memo, snapshot, built))
    }

    fn lookup(memo: &Mutex<Self>, snapshot: &Arc<CatalogSnapshot>) -> Option<Arc<T>> {
        let mut memo = crate::sync::lock_recover(memo);
        let hit = memo.position_of(snapshot)?;
        let entry = memo.entries.remove(hit);
        let value = Arc::clone(&entry.1);
        memo.entries.insert(0, entry);
        Some(value)
    }

    fn insert(memo: &Mutex<Self>, snapshot: &Arc<CatalogSnapshot>, built: Arc<T>) -> Arc<T> {
        let mut memo = crate::sync::lock_recover(memo);
        // Another thread may have inserted the same snapshot while we built ours;
        // converge on the existing canonical value if so.
        let value = memo.take(snapshot).unwrap_or(built);
        memo.entries
            .insert(0, (Arc::downgrade(snapshot), Arc::clone(&value)));
        while memo.entries.len() > memo.cap {
            memo.entries.pop();
        }
        value
    }

    /// Index of the entry whose snapshot is `Arc::ptr_eq` to `snapshot`, pruning
    /// any dead `Weak` entries encountered along the way.
    fn position_of(&mut self, snapshot: &Arc<CatalogSnapshot>) -> Option<usize> {
        let mut index = 0;
        while index < self.entries.len() {
            match self.entries[index].0.upgrade() {
                Some(existing) if Arc::ptr_eq(&existing, snapshot) => return Some(index),
                Some(_) => index += 1,
                None => {
                    self.entries.remove(index);
                }
            }
        }
        None
    }

    fn take(&mut self, snapshot: &Arc<CatalogSnapshot>) -> Option<Arc<T>> {
        let index = self.position_of(snapshot)?;
        Some(self.entries.remove(index).1)
    }
}
