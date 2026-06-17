use super::*;

#[test]
fn cancellable_json_dispatch_returns_cancelled_error_object() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("text.txt"), "needle\n").expect("write");
    let provider = FsCatalogProvider::new(
        RootPolicy::new(vec![dir.path().to_path_buf()]).expect("policy"),
        ScanOptions::default(),
    );
    let token = CancelToken::new();
    token.cancel();

    let json = handle_tool_call_json_cancellable(
        &provider,
        r#"{"name":"file_search","arguments":{"pattern":"needle","mode":"content"}}"#,
        &token,
    )
    .expect("cancelled dispatch is encoded as JSON");
    let value: Value = serde_json::from_str(&json).expect("json");
    assert_eq!(value["error"]["kind"], "cancelled");
}

#[test]
fn manage_selection_is_listed_dispatches_and_persists() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("text.txt"), "one\ntwo\n").expect("write");
    let provider = FsCatalogProvider::new(
        RootPolicy::new(vec![dir.path().to_path_buf()]).expect("policy"),
        ScanOptions::default(),
    );

    let specs = tool_specs();
    let tool_names: Vec<_> = specs
        .as_array()
        .expect("tool specs array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    assert!(tool_names.contains(&"manage_selection"));

    let set_response = handle_tool_call(
        &provider,
        &json!({
            "name": "manage_selection",
            "arguments": {
                "op": "set",
                "paths": ["text.txt"],
                "mode": "full"
            }
        }),
    )
    .expect("selection dispatch");
    assert_eq!(
        set_response["structuredContent"]["files"][0]["path"],
        Value::String("text.txt".to_string())
    );
    assert!(
        set_response["structuredContent"]["files"][0]["token_estimate"]
            .as_u64()
            .expect("token count")
            > 0
    );

    let get_response = handle_tool_call(
        &provider,
        &json!({
            "name": "manage_selection",
            "arguments": { "op": "get" }
        }),
    )
    .expect("selection get");
    assert_eq!(
        get_response["structuredContent"]["files"][0]["path"],
        Value::String("text.txt".to_string())
    );
}

#[test]
fn workspace_context_is_listed_and_dispatches() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("text.txt"), "one\ntwo\n").expect("write");
    let provider = FsCatalogProvider::new(
        RootPolicy::new(vec![dir.path().to_path_buf()]).expect("policy"),
        ScanOptions::default(),
    );

    let specs = tool_specs();
    let tool_names: Vec<_> = specs
        .as_array()
        .expect("tool specs array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    assert!(tool_names.contains(&"workspace_context"));

    handle_tool_call(
        &provider,
        &json!({
            "name": "manage_selection",
            "arguments": {
                "op": "set",
                "paths": ["text.txt"],
                "mode": "full"
            }
        }),
    )
    .expect("selection dispatch");

    let response = handle_tool_call(
        &provider,
        &json!({
            "name": "workspace_context",
            "arguments": {
                "include": ["file-map", "contents", "tokens"],
                "instructions": "Use this context."
            }
        }),
    )
    .expect("workspace context dispatch");
    // The assembled context lives in content[].text; structuredContent keeps
    // only the token breakdown (the body is not duplicated across channels).
    let text = response["content"][0]["text"].as_str().expect("text");
    assert!(text.contains("<file_map>"));
    assert!(text.contains("<tokens>"));
    let structured = &response["structuredContent"];
    assert!(structured["context"].is_null());
    assert_eq!(
        structured["tokens"]["files"][0]["path"],
        Value::String("text.txt".to_string())
    );
}

#[test]
fn build_context_is_listed_dispatches_and_preserves_selection() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("text.txt"), "needle\n").expect("write");
    let provider = FsCatalogProvider::new(
        RootPolicy::new(vec![dir.path().to_path_buf()]).expect("policy"),
        ScanOptions::default(),
    );
    let before = provider.selection();

    let specs = tool_specs();
    let tool_names: Vec<_> = specs
        .as_array()
        .expect("tool specs array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    assert!(tool_names.contains(&"build_context"));

    let response = handle_tool_call(
        &provider,
        &json!({
            "name": "build_context",
            "arguments": {
                "query": "needle",
                "token_budget": 100,
                "max_files": 1
            }
        }),
    )
    .expect("build context dispatch");
    let structured = &response["structuredContent"];
    assert_eq!(
        structured["manifest"]["included"][0]["path"],
        Value::String("text.txt".to_string())
    );
    assert_eq!(provider.selection(), before);
}
