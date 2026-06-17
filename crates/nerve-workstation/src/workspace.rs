use anyhow::{Context, Result, bail};
use clap::Args;
#[cfg(feature = "semantic")]
use nerve_core::semantic::{SemanticIndexScope, SemanticRuntimeConfig};
use nerve_core::{FsCatalogProvider, RootPolicy, ScanOptions, WorkspaceRegistry};
use std::{collections::BTreeMap, path::PathBuf, str::FromStr};

#[derive(Debug, Args, Clone)]
pub(crate) struct ServeArgs {
    /// Allowed root for the default workspace. Repeatable. `serve` fails closed when absent; `warm`/`cache purge` default to the current directory when no roots or workspaces are supplied.
    #[arg(long = "root")]
    pub(crate) roots: Vec<PathBuf>,
    /// Additional named workspace as name=path. Repeat to add workspaces or multiple roots per name.
    #[arg(long = "workspace")]
    pub(crate) workspaces: Vec<WorkspaceArg>,
    /// Maximum catalog entries per workspace.
    #[arg(long, default_value_t = 100_000)]
    pub(crate) max_entries: usize,
    /// Disable the built-in semantic_search index (on by default).
    #[cfg(feature = "semantic")]
    #[arg(long = "no-semantic")]
    pub(crate) no_semantic: bool,
    /// Embedding model name for semantic_search.
    #[cfg(feature = "semantic")]
    #[arg(long = "semantic-embedding-model")]
    pub(crate) semantic_embedding_model: Option<String>,
    /// Reranker model name for semantic_search.
    #[cfg(feature = "semantic")]
    #[arg(long = "semantic-reranker-model")]
    pub(crate) semantic_reranker_model: Option<String>,
    /// Model cache directory for semantic_search providers.
    #[cfg(feature = "semantic")]
    #[arg(long = "semantic-model-cache-dir")]
    pub(crate) semantic_model_cache_dir: Option<PathBuf>,
    /// Persistent semantic index cache directory.
    #[cfg(feature = "semantic")]
    #[arg(long = "semantic-cache-dir")]
    pub(crate) semantic_cache_dir: Option<PathBuf>,
    /// Enable semantic_search reranking (off by default). On local code corpora
    /// the available cross-encoder rerankers do not beat the fused BM25+dense
    /// ranking and add 15-20x query latency — see crates/nerve-core/tests/eval.rs.
    #[cfg(feature = "semantic")]
    #[arg(long = "semantic-rerank")]
    pub(crate) semantic_rerank: bool,
    /// Restrict semantic indexing to paths matching this glob. Repeatable.
    #[cfg(feature = "semantic")]
    #[arg(long = "semantic-include")]
    pub(crate) semantic_include: Vec<String>,
    /// Exclude paths from semantic indexing with this glob. Repeatable.
    #[cfg(feature = "semantic")]
    #[arg(long = "semantic-exclude")]
    pub(crate) semantic_exclude: Vec<String>,
    /// Restrict semantic indexing to this extension (dot optional). Repeatable.
    #[cfg(feature = "semantic")]
    #[arg(long = "semantic-extension")]
    pub(crate) semantic_extensions: Vec<String>,
    /// Do not apply the default semantic excludes for tests/docs/vendor/build/generated files.
    #[cfg(feature = "semantic")]
    #[arg(long = "semantic-no-default-excludes")]
    pub(crate) semantic_no_default_excludes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceArg {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

impl FromStr for WorkspaceArg {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let (name, path) = value
            .split_once('=')
            .ok_or_else(|| "workspace must be name=path".to_string())?;
        if name.is_empty() {
            return Err("workspace name must not be empty".to_string());
        }
        if path.is_empty() {
            return Err("workspace path must not be empty".to_string());
        }
        Ok(Self {
            name: name.to_string(),
            path: PathBuf::from(path),
        })
    }
}

pub(crate) fn scan_options(args: &ServeArgs) -> ScanOptions {
    ScanOptions {
        max_entries: args.max_entries,
        ..ScanOptions::default()
    }
}

#[cfg(feature = "semantic")]
pub(crate) fn semantic_runtime_config(args: &ServeArgs) -> SemanticRuntimeConfig {
    SemanticRuntimeConfig {
        enabled: !args.no_semantic,
        embedding_model: args.semantic_embedding_model.clone(),
        reranker_model: args.semantic_reranker_model.clone(),
        model_cache_dir: args.semantic_model_cache_dir.clone(),
        index_cache_dir: args.semantic_cache_dir.clone(),
        rerank: args.semantic_rerank,
        mock: false,
        scope: SemanticIndexScope {
            extensions: args.semantic_extensions.clone(),
            include: args.semantic_include.clone(),
            exclude: args.semantic_exclude.clone(),
            use_default_excludes: !args.semantic_no_default_excludes,
        },
    }
}

pub(crate) fn provider_for_roots(
    roots: Vec<PathBuf>,
    options: ScanOptions,
    args: &ServeArgs,
) -> Result<FsCatalogProvider> {
    let policy = RootPolicy::new(roots).context("invalid root policy")?;
    #[cfg(feature = "semantic")]
    {
        let semantic = semantic_runtime_config(args);
        let semantic_index = semantic
            .build_index_for_roots(policy.roots())
            .context("failed to initialize semantic index")?;
        Ok(FsCatalogProvider::with_semantic_index(
            policy,
            options,
            semantic_index,
        ))
    }
    #[cfg(not(feature = "semantic"))]
    {
        let _ = args;
        Ok(FsCatalogProvider::new(policy, options))
    }
}

pub(crate) fn registry(args: &ServeArgs) -> Result<WorkspaceRegistry> {
    let options = scan_options(args);
    #[cfg(feature = "semantic")]
    let registry: WorkspaceRegistry<FsCatalogProvider> =
        WorkspaceRegistry::with_scan_options_and_semantic(
            options.clone(),
            semantic_runtime_config(args),
        );
    #[cfg(not(feature = "semantic"))]
    let registry: WorkspaceRegistry<FsCatalogProvider> =
        WorkspaceRegistry::with_scan_options(options.clone());
    registry.insert(
        "default",
        std::sync::Arc::new(provider_for_roots(
            args.roots.clone(),
            options.clone(),
            args,
        )?),
    );

    let mut grouped: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for workspace in &args.workspaces {
        if workspace.name == "default" {
            bail!("--workspace default=... conflicts with --root default workspace");
        }
        grouped
            .entry(workspace.name.clone())
            .or_default()
            .push(workspace.path.clone());
    }
    for (name, roots) in grouped {
        registry.add_workspace(name, roots)?;
    }
    Ok(registry)
}

#[cfg(test)]
pub(crate) fn args_with(roots: Vec<PathBuf>, workspaces: Vec<WorkspaceArg>) -> ServeArgs {
    ServeArgs {
        roots,
        workspaces,
        max_entries: 100_000,
        #[cfg(feature = "semantic")]
        no_semantic: true,
        #[cfg(feature = "semantic")]
        semantic_embedding_model: None,
        #[cfg(feature = "semantic")]
        semantic_reranker_model: None,
        #[cfg(feature = "semantic")]
        semantic_model_cache_dir: None,
        #[cfg(feature = "semantic")]
        semantic_cache_dir: None,
        #[cfg(feature = "semantic")]
        semantic_rerank: false,
        #[cfg(feature = "semantic")]
        semantic_include: Vec::new(),
        #[cfg(feature = "semantic")]
        semantic_exclude: Vec::new(),
        #[cfg(feature = "semantic")]
        semantic_extensions: Vec::new(),
        #[cfg(feature = "semantic")]
        semantic_no_default_excludes: false,
    }
}
