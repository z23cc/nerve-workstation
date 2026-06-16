use super::{DispatchError, ToolText, edit};
use crate::{CatalogProvider, edit::FileChange};
use std::path::Path;

/// Adapts a [`CatalogProvider`] into an [`edit::FileReader`]; reads are
/// containment-checked by the provider's root policy.
pub(super) struct ProviderReader<'a, P: CatalogProvider + ?Sized> {
    pub(super) provider: &'a P,
}

impl<P: CatalogProvider + ?Sized> edit::FileReader for ProviderReader<'_, P> {
    fn read_text(&self, path: &str) -> Option<String> {
        self.provider
            .read_bytes(Path::new(path))
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }
}

#[derive(serde::Serialize)]
pub(super) struct EditedFile {
    pub(super) action: &'static str,
    pub(super) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) moved_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) view: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) diff: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) diagnostics: Vec<crate::codemap::SyntaxIssue>,
}

impl EditedFile {
    pub(super) fn with_content(
        action: &'static str,
        path: String,
        moved_to: Option<String>,
        content: &str,
        old: &str,
    ) -> Self {
        let display = moved_to.clone().unwrap_or_else(|| path.clone());
        let diff = (old != content).then(|| unified_diff(&display, old, content));
        Self {
            action,
            tag: Some(edit::snapshot_tag(content)),
            view: Some(edit::hashline_view(&display, content)),
            diff,
            diagnostics: crate::codemap::syntax_diagnostics(&display, content),
            path,
            moved_to,
        }
    }
}

#[derive(serde::Serialize)]
pub(super) struct EditResult {
    pub(super) files: Vec<EditedFile>,
}

impl ToolText for EditResult {
    fn tool_text(&self) -> String {
        let mut out = String::new();
        for file in &self.files {
            match &file.moved_to {
                Some(to) => out.push_str(&format!("{} {} -> {}\n", file.action, file.path, to)),
                None => out.push_str(&format!("{} {}\n", file.action, file.path)),
            }
        }
        for file in &self.files {
            for issue in &file.diagnostics {
                out.push_str(&format!(
                    "  \u{26a0} {} line {}: {}\n",
                    file.path, issue.line, issue.message
                ));
            }
        }
        for file in &self.files {
            if let Some(diff) = &file.diff {
                out.push('\n');
                out.push_str(diff);
            }
        }
        out
    }
}

pub(super) fn apply_changes<P: CatalogProvider + ?Sized>(
    provider: &P,
    changes: Vec<FileChange>,
) -> Result<EditResult, DispatchError> {
    let mut files = Vec::with_capacity(changes.len());
    for change in changes {
        let edited = match change {
            FileChange::Create { path, content } => {
                provider.write_text(Path::new(&path), &content)?;
                EditedFile::with_content("create", path, None, &content, "")
            }
            FileChange::Update { path, content } => {
                let old = read_old(provider, &path);
                provider.write_text(Path::new(&path), &content)?;
                EditedFile::with_content("update", path, None, &content, &old)
            }
            FileChange::Delete { path } => {
                provider.delete_file(Path::new(&path))?;
                EditedFile {
                    action: "delete",
                    path,
                    moved_to: None,
                    tag: None,
                    view: None,
                    diff: None,
                    diagnostics: Vec::new(),
                }
            }
            FileChange::Rename { from, to, content } => {
                let old = read_old(provider, &from);
                provider.rename_file(Path::new(&from), Path::new(&to))?;
                provider.write_text(Path::new(&to), &content)?;
                EditedFile::with_content("rename", from, Some(to), &content, &old)
            }
        };
        files.push(edited);
    }
    Ok(EditResult { files })
}

/// Current text of `path`, or empty if it does not exist / is unreadable.
pub(super) fn read_old<P: CatalogProvider + ?Sized>(provider: &P, path: &str) -> String {
    provider
        .read_bytes(Path::new(path))
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

/// A compact unified diff (old -> new) for the edit response.
pub(super) fn unified_diff(path: &str, old: &str, new: &str) -> String {
    let text_diff = similar::TextDiff::from_lines(old, new);
    let mut builder = text_diff.unified_diff();
    builder
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"));
    let rendered = builder.to_string();
    if rendered.chars().count() > 6000 {
        let capped: String = rendered.chars().take(6000).collect();
        format!("{capped}\n\u{2026} (diff truncated)\n")
    } else {
        rendered
    }
}
