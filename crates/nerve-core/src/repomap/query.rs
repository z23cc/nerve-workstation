use std::path::PathBuf;

pub(super) fn normalized_query(query: Option<&str>) -> Option<String> {
    query
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(ToString::to_string)
}

pub(super) fn normalize_seed_paths(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .map(|path| {
            path.trim_start_matches("./")
                .trim_end_matches('/')
                .to_string()
        })
        .filter(|path| !path.is_empty())
        .collect()
}

pub(super) fn query_matches(path: &str, source: &str, query: &str) -> bool {
    let case_sensitive = query.chars().any(char::is_uppercase);
    if case_sensitive {
        return path.contains(query) || source.contains(query);
    }
    let query = query.to_ascii_lowercase();
    path.to_ascii_lowercase().contains(&query) || source.to_ascii_lowercase().contains(&query)
}
