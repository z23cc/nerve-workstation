use crate::{
    cancel::CancelToken,
    codemap::symbols_for_path,
    models::*,
    port::{CatalogProvider, CodeSymbolsResult},
    selection::Selection,
    snapshot::CatalogSnapshot,
};
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    sync::{Arc, RwLock},
};

#[path = "memory_batch.rs"]
mod memory_batch;

/// One host-provided file for `MemoryCatalogProvider`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFile {
    pub path: PathBuf,
    pub content: Vec<u8>,
}

impl HostFile {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, content: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            content: content.into(),
        }
    }
}

/// Host-fed in-memory catalog provider.
///
/// This provider never scans the filesystem and never consults ignore rules. It
/// is intended for browser/edge wasm hosts that already possess file paths and
/// contents and want to feed them through the same engine port used by native
/// filesystem hosts.
#[derive(Clone, Debug)]
pub struct MemoryCatalogProvider {
    root_id: String,
    root_path: PathBuf,
    state: Arc<MemoryProviderState>,
}

#[derive(Debug)]
struct MemoryProviderState {
    snapshot: RwLock<Arc<CatalogSnapshot>>,
    files: RwLock<HashMap<PathBuf, Arc<Vec<u8>>>>,
    codemap: RwLock<HashMap<PathBuf, Arc<CodeSymbolsResult>>>,
    selection: RwLock<Selection>,
}

impl Default for MemoryProviderState {
    fn default() -> Self {
        Self {
            snapshot: RwLock::new(Arc::new(CatalogSnapshot {
                generation: 0,
                roots: Vec::new(),
                entries: Vec::new(),
                diagnostics: Vec::new(),
            })),
            files: RwLock::new(HashMap::new()),
            codemap: RwLock::new(HashMap::new()),
            selection: RwLock::new(Selection::default()),
        }
    }
}

impl Default for MemoryCatalogProvider {
    fn default() -> Self {
        Self::empty()
    }
}

impl MemoryCatalogProvider {
    const DEFAULT_ROOT_ID: &'static str = "memory-root";
    const DEFAULT_ROOT_NAME: &'static str = "host";

    #[must_use]
    pub fn empty() -> Self {
        let provider = Self {
            root_id: Self::DEFAULT_ROOT_ID.to_string(),
            root_path: PathBuf::from(Self::DEFAULT_ROOT_NAME),
            state: Arc::new(MemoryProviderState::default()),
        };
        provider
            .replace_files(Vec::new())
            .expect("empty host files");
        provider
    }

    pub fn new(files: Vec<HostFile>) -> Result<Self, NerveError> {
        let provider = Self::empty();
        provider.replace_files(files)?;
        Ok(provider)
    }

    pub fn from_pairs<I, P, C>(files: I) -> Result<Self, NerveError>
    where
        I: IntoIterator<Item = (P, C)>,
        P: Into<PathBuf>,
        C: Into<Vec<u8>>,
    {
        Self::new(
            files
                .into_iter()
                .map(|(path, content)| HostFile::new(path, content))
                .collect(),
        )
    }

    pub fn replace_files(&self, files: Vec<HostFile>) -> Result<(), NerveError> {
        let mut entries = Vec::with_capacity(files.len());
        let mut map = HashMap::with_capacity(files.len());

        for file in files {
            let normalized = normalize_host_path(&file.path)?;
            let rel_path = path_to_slash_string(&normalized);
            let content = Arc::new(file.content);
            entries.push(CatalogEntry {
                root_id: self.root_id.clone(),
                rel_path,
                abs_path: normalized.clone(),
                size: content.len() as u64,
            });
            map.insert(normalized, content);
        }

        entries.sort_by(|left, right| left.rel_path.cmp(&right.rel_path));
        let snapshot = CatalogSnapshot {
            generation: crate::sync::read_recover(&self.state.snapshot)
                .generation
                .saturating_add(1),
            roots: vec![RootRef {
                id: self.root_id.clone(),
                path: self.root_path.clone(),
            }],
            entries,
            diagnostics: Vec::new(),
        };

        *crate::sync::write_recover(&self.state.files) = map;
        *crate::sync::write_recover(&self.state.codemap) = HashMap::new();
        *crate::sync::write_recover(&self.state.snapshot) = Arc::new(snapshot);
        Ok(())
    }

    #[must_use]
    pub fn root_id(&self) -> &str {
        &self.root_id
    }
}

impl CatalogProvider for MemoryCatalogProvider {
    fn snapshot(&self) -> Result<CatalogSnapshot, NerveError> {
        Ok((**crate::sync::read_recover(&self.state.snapshot)).clone())
    }

    fn snapshot_arc(&self) -> Result<Arc<CatalogSnapshot>, NerveError> {
        Ok(Arc::clone(&crate::sync::read_recover(&self.state.snapshot)))
    }

    fn snapshot_arc_cancellable(
        &self,
        cancel: &CancelToken,
    ) -> Result<Arc<CatalogSnapshot>, NerveError> {
        cancel.check_cancelled()?;
        let snapshot = self.snapshot_arc()?;
        cancel.check_cancelled()?;
        Ok(snapshot)
    }

    fn invalidate(&self) {
        crate::sync::write_recover(&self.state.codemap).clear();
    }

    fn selection(&self) -> Selection {
        crate::sync::read_recover(&self.state.selection).clone()
    }

    fn set_selection(&self, selection: Selection) {
        *crate::sync::write_recover(&self.state.selection) = selection;
    }

    fn read_bytes(&self, path: &Path) -> Result<Vec<u8>, NerveError> {
        let normalized = normalize_host_path(path)?;
        crate::sync::read_recover(&self.state.files)
            .get(&normalized)
            .map(|bytes| bytes.as_ref().clone())
            .ok_or_else(|| NerveError::OutsideRoots(path.to_path_buf()))
    }

    fn validate_write_path(&self, path: &Path) -> Result<(), NerveError> {
        normalize_host_path(path).map(|_| ())
    }

    fn write_text(&self, path: &Path, content: &str) -> Result<(), NerveError> {
        self.apply_file_batch(
            &[crate::edit::FileChange::Update {
                path: path_to_slash_string(path),
                content: content.to_string(),
            }],
            false,
        )
    }

    fn delete_file(&self, path: &Path) -> Result<(), NerveError> {
        self.apply_file_batch(
            &[crate::edit::FileChange::Delete {
                path: path_to_slash_string(path),
            }],
            false,
        )
    }

    fn rename_file(&self, from: &Path, to: &Path) -> Result<(), NerveError> {
        let content = String::from_utf8_lossy(&self.read_bytes(from)?).into_owned();
        self.apply_file_batch(
            &[crate::edit::FileChange::Rename {
                from: path_to_slash_string(from),
                to: path_to_slash_string(to),
                content,
            }],
            false,
        )
    }

    fn apply_file_batch(
        &self,
        changes: &[crate::edit::FileChange],
        _atomic: bool,
    ) -> Result<(), NerveError> {
        let mut guard = crate::sync::write_recover(&self.state.files);
        let mut next = guard.clone();
        for change in changes {
            memory_batch::apply_change(&mut next, change)?;
        }
        *guard = next;
        let snapshot_files = guard.clone();
        drop(guard);
        memory_batch::refresh_snapshot_from_map(self, &snapshot_files);
        Ok(())
    }

    fn code_symbols_for_path(
        &self,
        path: &Path,
        rel_path: &str,
    ) -> Result<CodeSymbolsResult, NerveError> {
        let normalized = normalize_host_path(path)?;
        if let Some(cached) = crate::sync::read_recover(&self.state.codemap).get(&normalized) {
            return Ok((**cached).clone());
        }

        let bytes = self.read_bytes(&normalized)?;
        let source = String::from_utf8_lossy(&bytes);
        let parsed: CodeSymbolsResult =
            symbols_for_path(&source, rel_path).map(|maybe| maybe.map(Arc::new));
        crate::sync::write_recover(&self.state.codemap)
            .insert(normalized, Arc::new(parsed.clone()));
        Ok(parsed)
    }

    fn display_path(&self, path: &Path) -> String {
        normalize_host_path(path).map_or_else(
            |_| path.to_string_lossy().replace('\\', "/"),
            |normalized| {
                format!(
                    "{}/{}",
                    self.root_path.display(),
                    path_to_slash_string(&normalized)
                )
            },
        )
    }
}

fn normalize_host_path(path: &Path) -> Result<PathBuf, NerveError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                if normalized.as_os_str().is_empty()
                    && part == MemoryCatalogProvider::DEFAULT_ROOT_NAME
                {
                    continue;
                }
                normalized.push(part);
            }
            Component::CurDir | Component::RootDir => {}
            Component::ParentDir => {
                return Err(NerveError::PathTraversal(path.display().to_string()));
            }
            Component::Prefix(_) => {}
        }
    }
    if normalized.as_os_str().is_empty() {
        Err(NerveError::OutsideRoots(path.to_path_buf()))
    } else {
        Ok(normalized)
    }
}

fn path_to_slash_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
