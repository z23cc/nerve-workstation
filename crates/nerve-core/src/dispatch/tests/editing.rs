use super::*;

#[test]
fn edit_tools_modify_filesystem_within_roots() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.txt"), "alpha\nbeta\n").expect("seed");
    let provider = provider_for(dir.path());

    handle_tool_call(
        &provider,
        &json!({ "name": "write", "arguments": { "path": "b.txt", "content": "hello\n" } }),
    )
    .expect("write");
    assert_eq!(
        fs::read_to_string(dir.path().join("b.txt")).expect("b.txt"),
        "hello\n"
    );

    handle_tool_call(
        &provider,
        &json!({ "name": "write", "arguments": { "path": "a/b/c.txt", "content": "nested\n" } }),
    )
    .expect("nested write");
    assert_eq!(
        fs::read_to_string(dir.path().join("a/b/c.txt")).expect("nested file"),
        "nested\n"
    );
    assert!(dir.path().join("a/b").is_dir());

    handle_tool_call(
        &provider,
        &json!({ "name": "edit", "arguments": { "mode": "replace", "path": "a.txt",
            "edits": [{ "old_text": "alpha", "new_text": "ALPHA" }] } }),
    )
    .expect("edit replace");
    assert_eq!(
        fs::read_to_string(dir.path().join("a.txt")).expect("a.txt"),
        "ALPHA\nbeta\n"
    );

    let view = handle_tool_call(
        &provider,
        &json!({ "name": "read_file", "arguments": { "path": "a.txt", "view": "hashline" } }),
    )
    .expect("read hashline");
    let tag = view["structuredContent"]["hashline_tag"]
        .as_str()
        .expect("hashline_tag")
        .to_string();
    let patch = format!("*** Begin Patch\n[a.txt#{tag}]\nSWAP 2.=2:\n+BETA\n*** End Patch\n");
    handle_tool_call(
        &provider,
        &json!({ "name": "edit", "arguments": { "mode": "hashline", "patch": patch } }),
    )
    .expect("edit hashline");
    assert_eq!(
        fs::read_to_string(dir.path().join("a.txt")).expect("a.txt"),
        "ALPHA\nBETA\n"
    );

    handle_tool_call(
        &provider,
        &json!({ "name": "move", "arguments": { "from": "b.txt", "to": "c.txt" } }),
    )
    .expect("move");
    assert!(!dir.path().join("b.txt").exists());
    assert_eq!(
        fs::read_to_string(dir.path().join("c.txt")).expect("c.txt"),
        "hello\n"
    );

    handle_tool_call(
        &provider,
        &json!({ "name": "delete", "arguments": { "path": "c.txt" } }),
    )
    .expect("delete");
    assert!(!dir.path().join("c.txt").exists());
}

#[test]
fn apply_patch_duplicate_create_fails_preflight_without_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.txt"), "alpha\n").expect("seed");
    let provider = provider_for(dir.path());
    let changes = vec![
        edit::FileChange::Update {
            path: "a.txt".to_string(),
            content: "ALPHA\n".to_string(),
        },
        edit::FileChange::Create {
            path: "new.txt".to_string(),
            content: "one\n".to_string(),
        },
        edit::FileChange::Create {
            path: "new.txt".to_string(),
            content: "two\n".to_string(),
        },
    ];
    let err = apply_changes(&provider, changes, DiffOptions::default(), false)
        .err()
        .expect("duplicate create preflight");
    assert!(err.to_string().contains("duplicate create/update target"));
    assert_eq!(
        fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "alpha\n"
    );
    assert!(!dir.path().join("new.txt").exists());
}

#[test]
fn create_over_existing_fails_preflight_before_later_update() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.txt"), "alpha\n").expect("seed a");
    fs::write(dir.path().join("exists.txt"), "old\n").expect("seed exists");
    let provider = provider_for(dir.path());
    let changes = vec![
        edit::FileChange::Create {
            path: "exists.txt".to_string(),
            content: "new\n".to_string(),
        },
        edit::FileChange::Update {
            path: "a.txt".to_string(),
            content: "ALPHA\n".to_string(),
        },
    ];
    let err = apply_changes(&provider, changes, DiffOptions::default(), false)
        .err()
        .expect("existing destination preflight");
    assert!(err.to_string().contains("destination already exists"));
    assert_eq!(
        fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "alpha\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("exists.txt")).unwrap(),
        "old\n"
    );
}

#[test]
fn atomic_true_unsupported_provider_fails_before_mutation() {
    #[derive(Default)]
    struct BasicProvider(std::sync::RwLock<std::collections::BTreeMap<String, String>>);
    impl CatalogProvider for BasicProvider {
        fn snapshot(&self) -> Result<crate::CatalogSnapshot, NerveError> {
            Ok(crate::CatalogSnapshot {
                generation: 0,
                roots: vec![],
                entries: vec![],
                diagnostics: vec![],
            })
        }
        fn read_bytes(&self, path: &std::path::Path) -> Result<Vec<u8>, NerveError> {
            self.0
                .read()
                .unwrap()
                .get(&path.to_string_lossy().to_string())
                .map(|text| text.as_bytes().to_vec())
                .ok_or_else(|| NerveError::OutsideRoots(path.to_path_buf()))
        }
        fn write_text(&self, path: &std::path::Path, content: &str) -> Result<(), NerveError> {
            self.0
                .write()
                .unwrap()
                .insert(path.to_string_lossy().to_string(), content.to_string());
            Ok(())
        }
    }
    let provider = BasicProvider::default();
    provider
        .write_text(std::path::Path::new("a.txt"), "alpha\n")
        .unwrap();
    let changes = vec![edit::FileChange::Update {
        path: "a.txt".to_string(),
        content: "ALPHA\n".to_string(),
    }];
    let err = apply_changes(&provider, changes, DiffOptions::default(), true)
        .err()
        .expect("atomic unsupported");
    assert!(matches!(
        err,
        DispatchError::Core(NerveError::AtomicBatchUnsupported)
    ));
    assert_eq!(
        provider.read_bytes(std::path::Path::new("a.txt")).unwrap(),
        b"alpha\n"
    );
}

#[test]
fn non_atomic_preflight_rejects_invalid_create_before_update() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.txt"), "alpha\n").expect("seed a");
    fs::write(dir.path().join("not_dir"), "file\n").expect("seed blocker");
    let provider = provider_for(dir.path());
    let changes = vec![
        edit::FileChange::Update {
            path: "a.txt".to_string(),
            content: "ALPHA\n".to_string(),
        },
        edit::FileChange::Create {
            path: "not_dir/new.txt".to_string(),
            content: "new\n".to_string(),
        },
    ];
    apply_changes(&provider, changes, DiffOptions::default(), false)
        .err()
        .expect("invalid destination preflight");
    assert_eq!(
        fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "alpha\n"
    );
}

#[test]
fn fs_provider_create_does_not_overwrite_existing_in_batch_api() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("exists.txt"), "old\n").expect("seed exists");
    let provider = provider_for(dir.path());
    let changes = [edit::FileChange::Create {
        path: "exists.txt".to_string(),
        content: "new\n".to_string(),
    }];
    assert!(provider.apply_file_batch(&changes, false).is_err());
    assert_eq!(
        fs::read_to_string(dir.path().join("exists.txt")).unwrap(),
        "old\n"
    );
    assert!(provider.apply_file_batch(&changes, true).is_err());
    assert_eq!(
        fs::read_to_string(dir.path().join("exists.txt")).unwrap(),
        "old\n"
    );
}

#[test]
fn fs_atomic_backup_collision_preserves_user_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.txt"), "alpha\n").expect("seed a");
    fs::create_dir(dir.path().join("existing_dir")).expect("seed blocker");
    let backup_name = format!(".a.txt.ctx-bak-{}-0-0", std::process::id());
    fs::write(dir.path().join(&backup_name), "do not delete\n").expect("seed backup collision");
    let provider = provider_for(dir.path());
    let changes = vec![
        edit::FileChange::Update {
            path: "a.txt".to_string(),
            content: "ALPHA\n".to_string(),
        },
        edit::FileChange::Create {
            path: "existing_dir".to_string(),
            content: "new\n".to_string(),
        },
    ];
    apply_changes(&provider, changes, DiffOptions::default(), true)
        .err()
        .expect("atomic rollback");
    assert_eq!(
        fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "alpha\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join(backup_name)).unwrap(),
        "do not delete\n"
    );
}

#[test]
fn fs_atomic_rollback_restores_first_write_after_later_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.txt"), "alpha\n").expect("seed a");
    fs::create_dir(dir.path().join("existing_dir")).expect("seed blocker");
    let provider = provider_for(dir.path());
    let changes = vec![
        edit::FileChange::Update {
            path: "a.txt".to_string(),
            content: "ALPHA\n".to_string(),
        },
        edit::FileChange::Create {
            path: "existing_dir".to_string(),
            content: "new\n".to_string(),
        },
    ];
    let err = apply_changes(&provider, changes, DiffOptions::default(), true)
        .err()
        .expect("atomic rollback");
    assert!(matches!(
        err,
        DispatchError::Core(NerveError::AtomicBatchFailed { .. })
    ));
    assert_eq!(
        fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "alpha\n"
    );
    assert!(dir.path().join("existing_dir").is_dir());
}
