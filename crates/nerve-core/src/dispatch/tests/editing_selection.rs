use super::*;

#[test]
fn stale_hash_error_has_structured_reread_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.txt"), "current\n").expect("seed");
    let provider = provider_for(dir.path());
    let patch = "*** Begin Patch\n[a.txt#0000]\nSWAP 1.=1:\n+x\n*** End Patch\n";
    let err = handle_tool_call(
        &provider,
        &json!({ "name": "edit", "arguments": { "mode": "hashline", "patch": patch } }),
    )
    .expect_err("stale hash");
    let value = dispatch_error_value(&err);
    assert_eq!(value["error"]["kind"], json!("stale_hash"));
    assert_eq!(value["error"]["path"], json!("a.txt"));
    assert_eq!(value["error"]["expected_hash"], json!("0000"));
    assert!(value["error"]["actual_hash"].is_string());
    assert!(
        value["error"]["reread_hint"]
            .as_str()
            .unwrap()
            .contains("hashline")
    );
    assert!(value["error"].get("content").is_none());
    assert_eq!(
        fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "current\n"
    );
}

#[test]
fn selection_slices_rebase_across_mutation_tools() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\nfour\n").expect("seed a");
    fs::write(dir.path().join("w.txt"), "alpha\nbeta\ngamma\n").expect("seed w");
    fs::write(
        dir.path().join("a.rs"),
        "fn main() {\n    foo();\n    selected();\n}\n",
    )
    .expect("seed rs");
    fs::write(dir.path().join("m.txt"), "move me\n").expect("seed m");
    let provider = provider_for(dir.path());

    set_slice_selection(&provider, "a.txt", 3, 3);
    let edit = handle_tool_call(
        &provider,
        &json!({ "name": "edit", "arguments": { "mode": "replace", "path": "a.txt",
            "edits": [{ "old_text": "one\n", "new_text": "zero\none\n" }] } }),
    )
    .expect("edit replace");
    assert_eq!(
        edit["structuredContent"]["files"][0]["selection"]["ranges_after"][0]["start_line"],
        json!(4)
    );

    set_slice_selection(&provider, "w.txt", 2, 2);
    let write = handle_tool_call(
        &provider,
        &json!({ "name": "write", "arguments": { "path": "w.txt", "content": "replacement\n" } }),
    )
    .expect("write");
    assert_eq!(
        write["structuredContent"]["files"][0]["selection"]["dropped"][0]["start_line"],
        json!(2)
    );
    let selection = selection_response(&provider);
    assert!(
        selection["structuredContent"]["files"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    set_slice_selection(&provider, "a.rs", 3, 3);
    handle_tool_call(
        &provider,
        &json!({ "name": "ast_edit", "arguments": {
            "path": "a.rs", "mode": "pattern", "pattern": "foo()",
            "replacement": "foo();\n    inserted()" } }),
    )
    .expect("ast edit");
    let selection = selection_response(&provider);
    assert_eq!(
        selection["structuredContent"]["files"][0]["ranges"][0]["start_line"],
        json!(4)
    );

    set_full_selection(&provider, "m.txt");
    handle_tool_call(
        &provider,
        &json!({ "name": "move", "arguments": { "from": "m.txt", "to": "moved.txt" } }),
    )
    .expect("move selected");
    let moved = selection_response(&provider);
    assert_eq!(
        moved["structuredContent"]["files"][0]["path"],
        json!("moved.txt")
    );

    handle_tool_call(
        &provider,
        &json!({ "name": "delete", "arguments": { "path": "moved.txt" } }),
    )
    .expect("delete selected");
    let deleted = selection_response(&provider);
    assert!(
        deleted["structuredContent"]["files"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn move_over_selected_destination_drops_stale_destination_slice() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("src.txt"), "new\ncontent\n").expect("seed src");
    fs::write(dir.path().join("dst.txt"), "old\nselected\n").expect("seed dst");
    let provider = provider_for(dir.path());
    set_slice_selection(&provider, "dst.txt", 2, 2);

    let moved = handle_tool_call(
        &provider,
        &json!({ "name": "move", "arguments": { "from": "src.txt", "to": "dst.txt" } }),
    )
    .expect("move over selected destination");
    assert_eq!(
        moved["structuredContent"]["files"][0]["selection"]["dropped"][0]["start_line"],
        json!(2)
    );
    let selection = selection_response(&provider);
    assert!(
        selection["structuredContent"]["files"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn write_outside_roots_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = provider_for(dir.path());
    let result = handle_tool_call(
        &provider,
        &json!({ "name": "write", "arguments": { "path": "../escape.txt", "content": "x" } }),
    );
    assert!(result.is_err(), "writes outside roots must be rejected");
}

#[test]
fn edit_reports_syntax_diagnostics_on_broken_rust() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.rs"), "fn main() {\n    let x = 1;\n}\n").expect("seed");
    let provider = provider_for(dir.path());
    // Drop the closing brace to break the syntax.
    let result = handle_tool_call(
        &provider,
        &json!({ "name": "edit", "arguments": { "mode": "replace", "path": "a.rs",
            "edits": [{ "old_text": "}\n", "new_text": "\n" }] } }),
    )
    .expect("edit");
    let diagnostics = result["structuredContent"]["files"][0]["diagnostics"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !diagnostics.is_empty(),
        "expected syntax diagnostics for broken Rust"
    );
}

#[test]
fn ast_search_and_edit_tools() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.rs"), "fn main() { foo(); bar(); }\n").expect("seed");
    let provider = provider_for(dir.path());

    let res = handle_tool_call(
        &provider,
        &json!({ "name": "ast_search", "arguments": {
            "language": "rust",
            "query": "(call_expression function: (identifier) @name) @match" } }),
    )
    .expect("ast_search");
    assert_eq!(
        res["structuredContent"]["matches"]
            .as_array()
            .map(|matches| matches.len()),
        Some(2)
    );

    handle_tool_call(
        &provider,
        &json!({ "name": "ast_edit", "arguments": {
            "path": "a.rs",
            "query": "(call_expression) @match",
            "replacement": "done()" } }),
    )
    .expect("ast_edit");
    assert_eq!(
        fs::read_to_string(dir.path().join("a.rs")).expect("read"),
        "fn main() { done(); done(); }\n"
    );
}

#[test]
fn ast_pattern_search_and_edit_tools() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("a.rs"),
        "fn main() { foo(one); bar(one); }\n",
    )
    .expect("seed");
    let provider = provider_for(dir.path());

    let res = handle_tool_call(
        &provider,
        &json!({ "name": "ast_search", "arguments": {
            "language": "rust",
            "mode": "pattern",
            "pattern": "foo($ARG)" } }),
    )
    .expect("ast_search");
    assert_eq!(
        res["structuredContent"]["matches"]
            .as_array()
            .map(|matches| matches.len()),
        Some(1)
    );
    assert_eq!(
        res["structuredContent"]["matches"][0]["captures"]["ARG"],
        "one"
    );

    handle_tool_call(
        &provider,
        &json!({ "name": "ast_edit", "arguments": {
            "path": "a.rs",
            "mode": "pattern",
            "pattern": "foo($ARG)",
            "replacement": "baz(${ARG})" } }),
    )
    .expect("ast_edit");
    assert_eq!(
        fs::read_to_string(dir.path().join("a.rs")).expect("read"),
        "fn main() { baz(one); bar(one); }\n"
    );
}

#[test]
fn git_tool_and_per_edit_diff() {
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        return; // git not installed; skip
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("git");
    };
    git(&["init", "-q"]);
    fs::write(dir.path().join("a.txt"), "one\ntwo\n").expect("seed");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);
    let provider = provider_for(dir.path());

    // edit response carries a unified diff of exactly this change
    let res = handle_tool_call(
        &provider,
        &json!({ "name": "edit", "arguments": { "mode": "replace", "path": "a.txt",
            "edits": [{ "old_text": "two", "new_text": "TWO" }] } }),
    )
    .expect("edit");
    let diff = res["structuredContent"]["files"][0]["diff"]
        .as_str()
        .unwrap_or("");
    assert!(
        diff.contains("-two") && diff.contains("+TWO"),
        "diff: {diff}"
    );

    // git diff sees the working-tree change
    let g = handle_tool_call(
        &provider,
        &json!({ "name": "git", "arguments": { "op": "diff" } }),
    )
    .expect("git diff");
    assert!(
        g["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("+TWO"),
        "git diff output"
    );

    // git status lists the modified file
    let s = handle_tool_call(
        &provider,
        &json!({ "name": "git", "arguments": { "op": "status" } }),
    )
    .expect("git status");
    assert!(
        s["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("a.txt")
    );
}
