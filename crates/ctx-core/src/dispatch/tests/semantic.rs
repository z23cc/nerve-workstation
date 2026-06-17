use super::*;

#[cfg(all(feature = "semantic", not(target_arch = "wasm32")))]
#[test]
fn semantic_search_is_listed_when_feature_enabled() {
    let specs = tool_specs();
    let names: Vec<_> = specs
        .as_array()
        .expect("tool specs array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    assert!(names.contains(&"semantic_search"));
}

#[cfg(all(feature = "semantic", not(target_arch = "wasm32")))]
#[test]
fn semantic_search_without_runtime_index_is_unavailable() {
    let provider =
        MemoryCatalogProvider::new(vec![HostFile::new("a.rs", b"fn alpha() {}".to_vec())])
            .expect("provider");
    let err = handle_tool_call(
        &provider,
        &json!({
            "name": "semantic_search",
            "arguments": { "query": "alpha" }
        }),
    )
    .expect_err("semantic unavailable");
    assert!(matches!(
        err,
        DispatchError::Core(CtxError::SemanticUnavailable)
    ));
}

#[cfg(all(feature = "semantic", not(target_arch = "wasm32")))]
#[test]
fn semantic_search_dispatches_with_mock_index() {
    let provider = MemoryCatalogProvider::new(vec![
        HostFile::new("config.rs", b"pub fn validate_config() {}".to_vec()),
        HostFile::new("view.rs", b"pub fn render_view() {}".to_vec()),
    ])
    .expect("provider");
    provider.set_semantic_index(Some(Arc::new(SemanticIndex::mock())));
    let response = handle_tool_call(
        &provider,
        &json!({
            "name": "semantic_search",
            "arguments": { "query": "config validation", "max_results": "1" }
        }),
    )
    .expect("semantic search");
    assert_eq!(
        response["structuredContent"]["results"][0]["path"],
        Value::String("config.rs".to_string())
    );
    assert!(
        response["content"][0]["text"]
            .as_str()
            .expect("text")
            .contains("semantic matches:")
    );
}
