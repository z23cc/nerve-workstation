//! Workspace-context snapshot assembly from the persistent selection.

use crate::{
    codemap::FileCodeStructure,
    models::{CatalogEntry, NerveError},
    port::CatalogProvider,
    recipe::MetaPrompt,
    selection::{LineRange, Selection, SelectionKey, SelectionMode},
    snapshot::CatalogSnapshot,
    token::count_tokens,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Optional text sections to include in the assembled context text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceContextInclude {
    #[serde(rename = "file-map")]
    FileMap,
    Contents,
    Tokens,
    #[serde(rename = "git-diff")]
    GitDiff,
    #[serde(rename = "meta-prompts")]
    MetaPrompts,
}

/// Request for the `workspace_context` snapshot tool.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct WorkspaceContextRequest {
    #[serde(default)]
    pub include: Vec<WorkspaceContextInclude>,
    pub instructions: Option<String>,
    /// A named recipe (standard|plan|review|diff|manual). When set (and not
    /// `manual`) it fixes the section set, overriding `include`.
    #[serde(default)]
    pub recipe: Option<String>,
    /// Working-tree diff text to render in the `<git_diff>` section. The caller
    /// supplies it (e.g. from the `git` tool); the kernel never runs git.
    #[serde(default)]
    pub git_diff: Option<String>,
    /// Reusable instruction blocks, rendered as numbered `<meta prompt>` sections.
    /// When empty, a recipe's default meta-prompts (Plan/Review) are used.
    #[serde(default)]
    pub meta_prompts: Vec<MetaPrompt>,
}

/// Structured response for `workspace_context`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceContextResponse {
    /// Assembled context text. Not serialized into `structuredContent`: it is
    /// already the tool's `content[].text`, so emitting it twice would double
    /// the payload. The token breakdown stays structured.
    #[serde(default, skip_serializing)]
    pub context: String,
    pub tokens: WorkspaceContextTokenBreakdown,
}

/// Token breakdown for the assembled workspace context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceContextTokenBreakdown {
    /// Total tokens for the assembled payload excluding the optional token report.
    pub total_tokens: usize,
    pub file_map_tokens: usize,
    pub instructions_tokens: usize,
    pub contents_tokens: usize,
    pub git_diff_tokens: usize,
    pub meta_prompts_tokens: usize,
    pub files: Vec<WorkspaceContextFileTokens>,
}

/// Per-file token breakdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceContextFileTokens {
    pub root_id: String,
    pub path: String,
    pub display_path: String,
    pub mode: String,
    pub token_count: usize,
    pub segments: Vec<WorkspaceContextSegmentTokens>,
}

/// Per-segment token breakdown for slices and file sections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceContextSegmentTokens {
    pub label: String,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub token_count: usize,
}

/// Assemble the current provider selection into a context snapshot.
pub fn workspace_context<P: CatalogProvider>(
    provider: &P,
    snapshot: &CatalogSnapshot,
    request: &WorkspaceContextRequest,
) -> Result<WorkspaceContextResponse, NerveError> {
    let selection = provider.selection();
    workspace_context_for_selection(provider, snapshot, &selection, request)
}

/// Assemble an explicit selection without mutating provider-owned state.
pub fn workspace_context_for_selection<P: CatalogProvider>(
    provider: &P,
    snapshot: &CatalogSnapshot,
    selection: &Selection,
    request: &WorkspaceContextRequest,
) -> Result<WorkspaceContextResponse, NerveError> {
    let mut cache = RenderCache::new(snapshot.generation);
    workspace_context_for_selection_cached(provider, snapshot, selection, request, &mut cache)
}

/// Per-file render memoization for repeated `workspace_context` assembly.
///
/// `build_context`'s greedy allocator assembles many overlapping selections
/// that differ by a single trial file. Rendering each already-selected file
/// from scratch on every trial is `O(files²)`. Rendered text and its token
/// breakdown are a pure function of `(SelectionKey, SelectionMode, generation)`,
/// so they are cached here and reused byte-for-byte across trials. The final
/// `count_tokens` over the assembled context is *not* cached: BPE is not
/// additive across the `\n\n` joins, so the whole-string count is the only
/// faithful total and must be recomputed per assembly.
pub(crate) struct RenderCache {
    generation: u64,
    entries: BTreeMap<(SelectionKey, SelectionMode), RenderedFile>,
}

impl RenderCache {
    pub(crate) fn new(generation: u64) -> Self {
        Self {
            generation,
            entries: BTreeMap::new(),
        }
    }
}

/// Assemble an explicit selection, memoizing per-file rendering across calls.
pub(crate) fn workspace_context_for_selection_cached<P: CatalogProvider>(
    provider: &P,
    snapshot: &CatalogSnapshot,
    selection: &Selection,
    request: &WorkspaceContextRequest,
    cache: &mut RenderCache,
) -> Result<WorkspaceContextResponse, NerveError> {
    debug_assert_eq!(
        cache.generation, snapshot.generation,
        "RenderCache reused across snapshot generations"
    );
    let include = IncludeSet::from_request(request);
    let selected = selected_entries(provider, snapshot, selection);

    let file_map = render_file_map(&selected);
    let file_map_tokens = if include.file_map {
        count_tokens(&file_map)
    } else {
        0
    };

    let mut file_tokens = Vec::new();
    let mut rendered_files = Vec::new();
    for selected_file in selected {
        let rendered = render_selected_file_cached(provider, &selected_file, cache)?;
        file_tokens.push(rendered.tokens);
        rendered_files.push(rendered.text);
    }

    let contents_text = rendered_files.join("\n\n");
    let contents_tokens = if include.contents {
        file_tokens.iter().map(|file| file.token_count).sum()
    } else {
        0
    };

    // git_diff section: caller-supplied text only — the kernel never runs git.
    let git_diff = include
        .git_diff
        .then_some(request.git_diff.as_deref())
        .flatten()
        .filter(|diff| !diff.trim().is_empty())
        .map(render_git_diff);
    let git_diff_tokens = git_diff.as_deref().map_or(0, count_tokens);

    // meta-prompts: caller-supplied, else the named recipe's defaults.
    let meta_prompts = if include.meta_prompts {
        resolve_meta_prompts(request)
    } else {
        Vec::new()
    };
    let meta_prompts_text = (!meta_prompts.is_empty()).then(|| render_meta_prompts(&meta_prompts));
    let meta_prompts_tokens = meta_prompts_text.as_deref().map_or(0, count_tokens);

    let instructions = request.instructions.as_deref().map(render_instructions);
    let instructions_tokens = instructions.as_deref().map_or(0, count_tokens);

    // Ordered sections: file_map, file_contents, git_diff, meta_prompts,
    // instructions (mirrors RepoPrompt's section order — instructions last).
    let mut sections = Vec::new();
    if include.file_map {
        sections.push(file_map);
    }
    if include.contents && !contents_text.is_empty() {
        sections.push(contents_text);
    }
    if let Some(git_diff) = git_diff {
        sections.push(git_diff);
    }
    if let Some(meta_prompts_text) = meta_prompts_text {
        sections.push(meta_prompts_text);
    }
    if let Some(instructions) = instructions {
        sections.push(instructions);
    }

    let context = sections.join("\n\n");
    let total_tokens = count_tokens(&context);
    let mut response = WorkspaceContextResponse {
        context,
        tokens: WorkspaceContextTokenBreakdown {
            total_tokens,
            file_map_tokens,
            instructions_tokens,
            contents_tokens,
            git_diff_tokens,
            meta_prompts_tokens,
            files: file_tokens,
        },
    };

    if include.tokens {
        let token_report =
            serde_json::to_string_pretty(&response.tokens).expect("token breakdown serializes");
        if response.context.is_empty() {
            response.context = format!("<tokens>\n{token_report}\n</tokens>");
        } else {
            response
                .context
                .push_str(&format!("\n\n<tokens>\n{token_report}\n</tokens>"));
        }
    }

    Ok(response)
}

#[derive(Debug)]
struct IncludeSet {
    file_map: bool,
    contents: bool,
    tokens: bool,
    git_diff: bool,
    meta_prompts: bool,
}

impl IncludeSet {
    fn from_request(request: &WorkspaceContextRequest) -> Self {
        use WorkspaceContextInclude::{Contents, FileMap, GitDiff, MetaPrompts, Tokens};
        // A named recipe (except `manual`) fixes the section set, overriding `include`.
        if let Some(recipe) = request
            .recipe
            .as_deref()
            .filter(|name| *name != "manual")
            .and_then(crate::recipe::recipe_by_name)
        {
            let has = |section| recipe.sections.contains(&section);
            return Self {
                file_map: has(FileMap),
                contents: has(Contents),
                git_diff: has(GitDiff),
                meta_prompts: has(MetaPrompts),
                tokens: has(Tokens),
            };
        }
        if request.include.is_empty() {
            return Self {
                file_map: true,
                contents: true,
                tokens: false,
                git_diff: false,
                meta_prompts: false,
            };
        }
        Self {
            file_map: request.include.contains(&FileMap),
            contents: request.include.contains(&Contents),
            tokens: request.include.contains(&Tokens),
            git_diff: request.include.contains(&GitDiff),
            meta_prompts: request.include.contains(&MetaPrompts),
        }
    }
}

#[derive(Debug)]
struct SelectedFile<'a> {
    key: SelectionKey,
    entry: &'a CatalogEntry,
    display_path: String,
    mode: SelectionMode,
}

#[derive(Debug, Clone)]
struct RenderedFile {
    text: String,
    tokens: WorkspaceContextFileTokens,
}

fn selected_entries<'a, P: CatalogProvider>(
    provider: &P,
    snapshot: &'a CatalogSnapshot,
    selection: &Selection,
) -> Vec<SelectedFile<'a>> {
    let entries_by_key = snapshot
        .entries
        .iter()
        .map(|entry| (selection_key(entry), entry))
        .collect::<BTreeMap<_, _>>();

    selection
        .files
        .clone()
        .into_iter()
        .filter_map(|(key, mode)| {
            entries_by_key.get(&key).map(|entry| SelectedFile {
                key,
                entry,
                display_path: provider.display_path(&entry.abs_path),
                mode,
            })
        })
        .collect()
}

fn render_file_map(files: &[SelectedFile<'_>]) -> String {
    let mut lines = vec!["<file_map>".to_string()];
    for file in files {
        lines.push(format!(
            "- {} [{}]",
            file.display_path,
            mode_name(&file.mode)
        ));
    }
    lines.push("</file_map>".to_string());
    lines.join("\n")
}

fn render_instructions(instructions: &str) -> String {
    format!("<instructions>\n{instructions}\n</instructions>")
}

/// Render the caller-supplied working-tree diff as a `<git_diff>` section.
fn render_git_diff(diff: &str) -> String {
    format!("<git_diff>\n{}\n</git_diff>", diff.trim_end())
}

/// Render reusable instruction blocks as numbered `<meta prompt N="title">` sections.
fn render_meta_prompts(prompts: &[MetaPrompt]) -> String {
    prompts
        .iter()
        .enumerate()
        .map(|(i, prompt)| {
            let n = i + 1;
            format!(
                "<meta prompt {n}=\"{title}\">\n{body}\n</meta prompt {n}>",
                title = prompt.title,
                body = prompt.body
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Caller-supplied meta-prompts, or the named recipe's defaults when none given.
fn resolve_meta_prompts(request: &WorkspaceContextRequest) -> Vec<MetaPrompt> {
    if !request.meta_prompts.is_empty() {
        return request.meta_prompts.clone();
    }
    request
        .recipe
        .as_deref()
        .and_then(crate::recipe::recipe_by_name)
        .map(|recipe| {
            recipe
                .meta_prompts
                .iter()
                .map(|(title, body)| MetaPrompt {
                    title: (*title).to_string(),
                    body: (*body).to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn render_selected_file<P: CatalogProvider>(
    provider: &P,
    selected: &SelectedFile<'_>,
) -> Result<RenderedFile, NerveError> {
    match &selected.mode {
        SelectionMode::Full => render_full_file(provider, selected),
        SelectionMode::Slices(ranges) => render_slices_file(provider, selected, ranges),
        SelectionMode::CodemapOnly => render_codemap_file(provider, selected),
    }
}

fn render_selected_file_cached<P: CatalogProvider>(
    provider: &P,
    selected: &SelectedFile<'_>,
    cache: &mut RenderCache,
) -> Result<RenderedFile, NerveError> {
    let key = (selected.key.clone(), selected.mode.clone());
    if let Some(rendered) = cache.entries.get(&key) {
        return Ok(rendered.clone());
    }
    let rendered = render_selected_file(provider, selected)?;
    cache.entries.insert(key, rendered.clone());
    Ok(rendered)
}

fn render_full_file<P: CatalogProvider>(
    provider: &P,
    selected: &SelectedFile<'_>,
) -> Result<RenderedFile, NerveError> {
    let bytes = provider.read_bytes(&selected.entry.abs_path)?;
    let content = String::from_utf8_lossy(&bytes);
    let segment_tokens = count_tokens(&content);
    let text = format!(
        "<file path=\"{}\" mode=\"full\">\n```text\n{}```\n</file>",
        selected.display_path, content
    );
    let file_token_count = count_tokens(&text);
    Ok(RenderedFile {
        text,
        tokens: file_tokens(
            selected,
            file_token_count,
            vec![WorkspaceContextSegmentTokens {
                label: "full".to_string(),
                start_line: Some(1),
                end_line: Some(total_lines(&content)),
                token_count: segment_tokens,
            }],
        ),
    })
}

fn render_slices_file<P: CatalogProvider>(
    provider: &P,
    selected: &SelectedFile<'_>,
    ranges: &[LineRange],
) -> Result<RenderedFile, NerveError> {
    let bytes = provider.read_bytes(&selected.entry.abs_path)?;
    let content = String::from_utf8_lossy(&bytes);
    let mut text = format!(
        "<file path=\"{}\" mode=\"slices\">\n",
        selected.display_path
    );
    let mut segments = Vec::new();
    for range in ranges {
        let slice = slice_text(&content, range);
        let label = format!("lines {}-{}", range.start_line, range.end_line);
        let token_count = count_tokens(&slice);
        text.push_str(&format!(
            "<slice lines=\"{}-{}\" description=\"{}\">\n```text\n{}```\n</slice>\n",
            range.start_line, range.end_line, label, slice
        ));
        segments.push(WorkspaceContextSegmentTokens {
            label,
            start_line: Some(range.start_line),
            end_line: Some(range.end_line),
            token_count,
        });
    }
    text.push_str("</file>");
    let token_count = count_tokens(&text);
    Ok(RenderedFile {
        text,
        tokens: file_tokens(selected, token_count, segments),
    })
}

fn render_codemap_file<P: CatalogProvider>(
    provider: &P,
    selected: &SelectedFile<'_>,
) -> Result<RenderedFile, NerveError> {
    let (codemap_text, segment_tokens) =
        match provider.code_symbols_for_path(&selected.entry.abs_path, &selected.entry.rel_path)? {
            Ok(Some(parsed)) => {
                let structure = FileCodeStructure {
                    path: selected.entry.rel_path.clone(),
                    language: parsed.language.clone(),
                    symbols: parsed.symbols.clone(),
                    token_count: 0,
                };
                let text = render_codemap_signature(&structure);
                let tokens = count_tokens(&text);
                (text, tokens)
            }
            Ok(None) => ("unsupported file for codemap\n".to_string(), 0),
            Err(message) => (format!("codemap error: {message}\n"), 0),
        };
    let text = format!(
        "<file path=\"{}\" mode=\"codemap_only\">\n```text\n{}```\n</file>",
        selected.display_path, codemap_text
    );
    let file_token_count = count_tokens(&text);
    Ok(RenderedFile {
        text,
        tokens: file_tokens(
            selected,
            file_token_count,
            vec![WorkspaceContextSegmentTokens {
                label: "codemap".to_string(),
                start_line: None,
                end_line: None,
                token_count: segment_tokens,
            }],
        ),
    })
}

fn render_codemap_signature(structure: &FileCodeStructure) -> String {
    let mut lines = vec![format!("language: {}", structure.language)];
    for symbol in &structure.symbols {
        lines.push(format!(
            "- {} {} @ line {}",
            symbol.kind, symbol.name, symbol.line
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn slice_text(text: &str, range: &LineRange) -> String {
    let line_segments: Vec<&str> = text.split_inclusive('\n').collect();
    if line_segments.is_empty() {
        return String::new();
    }
    let start = range.start_line.max(1).min(line_segments.len());
    let end = range.end_line.max(start).min(line_segments.len());
    line_segments[start - 1..end].concat()
}

fn total_lines(text: &str) -> usize {
    text.split_inclusive('\n').count().max(1)
}

fn file_tokens(
    selected: &SelectedFile<'_>,
    token_count: usize,
    segments: Vec<WorkspaceContextSegmentTokens>,
) -> WorkspaceContextFileTokens {
    WorkspaceContextFileTokens {
        root_id: selected.key.root_id.clone(),
        path: selected.key.path.clone(),
        display_path: selected.display_path.clone(),
        mode: mode_name(&selected.mode).to_string(),
        token_count,
        segments,
    }
}

fn selection_key(entry: &CatalogEntry) -> SelectionKey {
    SelectionKey {
        root_id: entry.root_id.clone(),
        path: entry.rel_path.clone(),
    }
}

fn mode_name(mode: &SelectionMode) -> &'static str {
    match mode {
        SelectionMode::Full => "full",
        SelectionMode::Slices(_) => "slices",
        SelectionMode::CodemapOnly => "codemap_only",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        FsCatalogProvider, ManageSelectionMode, ManageSelectionOp, ManageSelectionRequest,
        RootPolicy, ScanOptions, SelectionSliceArg, manage_selection,
    };
    use std::{fs, path::PathBuf};

    fn provider_with_selection() -> (FsCatalogProvider, CatalogSnapshot) {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("full.txt"), "full file\n").expect("write");
        fs::write(dir.path().join("notes.txt"), "one\ntwo\nthree\n").expect("write");
        fs::write(dir.path().join("lib.rs"), "pub fn alpha() {}\n").expect("write");
        let path = dir.keep();
        let provider = FsCatalogProvider::new(
            RootPolicy::new(vec![path]).expect("policy"),
            ScanOptions::default(),
        );
        let snapshot = provider.snapshot().expect("snapshot");
        manage_selection(
            &provider,
            &snapshot,
            &ManageSelectionRequest {
                op: ManageSelectionOp::Set,
                paths: vec![PathBuf::from("full.txt")],
                mode: Some(ManageSelectionMode::Full),
                slices: Vec::new(),
            },
        )
        .expect("select full");
        manage_selection(
            &provider,
            &snapshot,
            &ManageSelectionRequest {
                op: ManageSelectionOp::Add,
                paths: Vec::new(),
                mode: Some(ManageSelectionMode::Slices),
                slices: vec![SelectionSliceArg {
                    path: PathBuf::from("notes.txt"),
                    ranges: vec![LineRange {
                        start_line: 2,
                        end_line: 2,
                    }],
                }],
            },
        )
        .expect("replace with slice");
        manage_selection(
            &provider,
            &snapshot,
            &ManageSelectionRequest {
                op: ManageSelectionOp::Add,
                paths: vec![PathBuf::from("lib.rs")],
                mode: Some(ManageSelectionMode::CodemapOnly),
                slices: Vec::new(),
            },
        )
        .expect("select codemap");
        (provider, snapshot)
    }

    #[test]
    fn renders_modes_and_token_breakdown() {
        let (provider, snapshot) = provider_with_selection();
        let response = workspace_context(
            &provider,
            &snapshot,
            &WorkspaceContextRequest {
                include: Vec::new(),
                instructions: Some("Use this context.".to_string()),
                ..Default::default()
            },
        )
        .expect("workspace context");

        assert!(response.context.contains("<file_map>"));
        assert!(response.context.contains("<instructions>"));
        assert!(response.context.contains("mode=\"full\""));
        assert!(response.context.contains("mode=\"slices\""));
        assert!(response.context.contains("description=\"lines 2-2\""));
        assert!(response.context.contains("mode=\"codemap_only\""));
        assert!(response.context.contains("- function alpha @ line 1"));
        assert_eq!(response.tokens.files.len(), 3);
        assert!(response.tokens.total_tokens > 0);
        assert!(
            response
                .tokens
                .files
                .iter()
                .any(|file| file.mode == "slices" && !file.segments.is_empty())
        );
    }

    #[test]
    fn include_can_omit_contents_from_context_text() {
        let (provider, snapshot) = provider_with_selection();
        let response = workspace_context(
            &provider,
            &snapshot,
            &WorkspaceContextRequest {
                include: vec![WorkspaceContextInclude::FileMap],
                instructions: None,
                ..Default::default()
            },
        )
        .expect("workspace context");

        assert!(response.context.contains("<file_map>"));
        assert!(!response.context.contains("<file path="));
        assert_eq!(response.tokens.contents_tokens, 0);
    }

    #[test]
    fn recipe_review_assembles_git_diff_and_default_meta_prompt() {
        let (provider, snapshot) = provider_with_selection();
        let response = workspace_context(
            &provider,
            &snapshot,
            &WorkspaceContextRequest {
                recipe: Some("review".to_string()),
                git_diff: Some("diff --git a/full.txt b/full.txt\n+added line".to_string()),
                ..Default::default()
            },
        )
        .expect("workspace context");

        // review = file_map + contents + git_diff + meta_prompts (default Review).
        assert!(response.context.contains("<file_map>"));
        assert!(response.context.contains("<git_diff>"));
        assert!(response.context.contains("diff --git"));
        assert!(response.context.contains("<meta prompt 1=\"Review\">"));
        assert!(response.tokens.git_diff_tokens > 0);
        assert!(response.tokens.meta_prompts_tokens > 0);
    }

    #[test]
    fn recipe_diff_is_git_only() {
        let (provider, snapshot) = provider_with_selection();
        let response = workspace_context(
            &provider,
            &snapshot,
            &WorkspaceContextRequest {
                recipe: Some("diff".to_string()),
                git_diff: Some("diff --git a/x b/x\n+y".to_string()),
                ..Default::default()
            },
        )
        .expect("workspace context");

        assert!(response.context.contains("<git_diff>"));
        assert!(!response.context.contains("<file_map>"));
        assert!(!response.context.contains("<meta prompt"));
        assert_eq!(response.tokens.file_map_tokens, 0);
        assert_eq!(response.tokens.contents_tokens, 0);
    }
}
