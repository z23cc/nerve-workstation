use super::ToolText;

#[derive(serde::Serialize)]
pub(super) struct AstFileMatch {
    pub(super) path: String,
    pub(super) line: usize,
    pub(super) text: String,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub(super) captures: std::collections::BTreeMap<String, String>,
}

#[derive(serde::Serialize)]
pub(super) struct AstSearchResponse {
    pub(super) matches: Vec<AstFileMatch>,
    pub(super) files_scanned: usize,
}

impl ToolText for AstSearchResponse {
    fn tool_text(&self) -> String {
        if self.matches.is_empty() {
            return format!("(no matches in {} files)\n", self.files_scanned);
        }
        let mut out = String::new();
        for item in &self.matches {
            out.push_str(&format!("{}:{}  {}\n", item.path, item.line, item.text));
        }
        out
    }
}

/// True if `rel_path` is `scope` itself or lives under directory `scope`.
pub(super) fn path_in_scope(rel_path: &str, scope: &str) -> bool {
    let scope = scope.trim_end_matches('/');
    scope.is_empty() || rel_path == scope || rel_path.starts_with(&format!("{scope}/"))
}
