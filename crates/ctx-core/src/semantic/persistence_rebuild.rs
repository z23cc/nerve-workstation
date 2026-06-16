use super::*;

pub(super) struct PersistenceRebuild {
    pub(super) records: Vec<SemanticChunkRecord>,
    pub(super) files: BTreeMap<String, PersistedFileRecord>,
    pub(super) changed_entries: Vec<CatalogEntry>,
    pub(super) diagnostics: Vec<Diagnostic>,
}

pub(super) fn load_or_clean_persisted_index(
    persistence: &SemanticPersistenceConfig,
) -> Option<LoadedPersistedIndex> {
    match load_persisted_index(persistence) {
        Ok(Some(index)) => Some(index),
        Ok(None) => None,
        Err(_) => {
            let _ = clean_workspace_cache(persistence);
            None
        }
    }
}

pub(super) fn reconcile_persisted_files(
    loaded: Option<&LoadedPersistedIndex>,
    state: &SnapshotFileState,
) -> PersistenceRebuild {
    let mut records = loaded
        .map(|index| index.records.clone())
        .unwrap_or_default();
    let mut old_files = loaded.map(|index| index.files.clone()).unwrap_or_default();
    let mut rebuild = PersistenceRebuild {
        records: Vec::new(),
        files: BTreeMap::new(),
        changed_entries: Vec::new(),
        diagnostics: Vec::new(),
    };
    std::mem::swap(&mut rebuild.records, &mut records);

    let mut current_keys = HashSet::new();
    for file in &state.files {
        current_keys.insert(file.key.clone());
        if retain_unchanged_file(file, &mut old_files, &mut rebuild.files) {
            continue;
        }
        tombstone_replaced_file(loaded, file, &mut rebuild.records);
        rebuild.changed_entries.push(file.entry.clone());
    }
    tombstone_removed_files(old_files, &current_keys, &mut rebuild.records);
    rebuild
}

pub(super) fn retain_unchanged_file(
    file: &SnapshotFileRecord,
    old_files: &mut BTreeMap<String, PersistedFileRecord>,
    files: &mut BTreeMap<String, PersistedFileRecord>,
) -> bool {
    let Some(old_file) = old_files.remove(&file.key) else {
        return false;
    };
    if old_file.signature != file.signature {
        old_files.insert(file.key.clone(), old_file);
        return false;
    }
    files.insert(file.key.clone(), old_file);
    true
}

pub(super) fn tombstone_replaced_file(
    loaded: Option<&LoadedPersistedIndex>,
    file: &SnapshotFileRecord,
    records: &mut [SemanticChunkRecord],
) {
    if let Some(old_file) = loaded.and_then(|index| index.files.get(&file.key)) {
        tombstone_chunks(records, &old_file.file_key, &old_file.chunk_ids);
    }
}

pub(super) fn tombstone_removed_files(
    old_files: BTreeMap<String, PersistedFileRecord>,
    current_keys: &HashSet<String>,
    records: &mut [SemanticChunkRecord],
) {
    for (_, removed) in old_files {
        if !current_keys.contains(&removed.file_key) {
            tombstone_chunks(records, &removed.file_key, &removed.chunk_ids);
        }
    }
}

pub(super) fn push_rebuilt_chunks(
    records: &mut Vec<SemanticChunkRecord>,
    chunks: Vec<SemanticChunk>,
    embeddings: Vec<Vec<f32>>,
    file_key: &str,
) {
    for (chunk, embedding) in chunks.into_iter().zip(embeddings) {
        records.push(SemanticChunkRecord {
            chunk,
            embedding,
            active: true,
            file_key: file_key.to_string(),
        });
    }
}

pub(super) fn rebuilt_file_record(
    entry: CatalogEntry,
    state: &SnapshotFileState,
    file_key: String,
    chunk_ids: Vec<String>,
) -> PersistedFileRecord {
    let signature = state
        .files
        .iter()
        .find(|file| file.key == file_key)
        .map(|file| file.signature.clone())
        .unwrap_or(PersistedFileSignature {
            modified_unix_nanos: None,
            size: entry.size,
        });
    PersistedFileRecord {
        file_key,
        root_id: entry.root_id,
        rel_path: entry.rel_path,
        signature,
        chunk_ids,
    }
}

pub(super) fn compact_records_if_needed(
    records: &mut Vec<SemanticChunkRecord>,
    config: &SemanticIndexConfig,
) {
    let tombstones = records.iter().filter(|record| !record.active).count();
    if should_compact(records.len(), tombstones, config) {
        records.retain(|record| record.active);
    }
}

pub(super) fn save_or_attach_cache_diagnostic(
    persistence: &SemanticPersistenceConfig,
    rebuild: PersistenceRebuild,
    built: BuiltSemanticIndex,
) -> Result<BuiltSemanticIndex, CtxError> {
    if let Err(err) = save_persisted_index(
        persistence,
        &rebuild.records,
        &rebuild.files,
        rebuild.diagnostics,
        &built.ann,
    ) {
        let mut with_diagnostic = built;
        with_diagnostic.diagnostics.push(Diagnostic {
            path: None,
            message: format!("semantic cache write failed; using in-memory index: {err}"),
        });
        return Ok(with_diagnostic);
    }
    Ok(built)
}
