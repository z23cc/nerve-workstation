use super::ServeArgs;
use anyhow::{Result, bail};

#[cfg(feature = "semantic")]
use super::{provider_for_roots, scan_options};
#[cfg(feature = "semantic")]
use anyhow::Context;
#[cfg(feature = "semantic")]
use ctx_core::semantic::SemanticWarmResponse;
#[cfg(feature = "semantic")]
use ctx_core::{CancelToken, CatalogProvider};
#[cfg(feature = "semantic")]
use std::{collections::BTreeMap, path::PathBuf, time::Instant};

#[cfg(feature = "semantic")]
#[derive(Debug)]
struct WorkspaceRoots {
    name: String,
    roots: Vec<PathBuf>,
}

pub(super) fn warm(args: ServeArgs) -> Result<()> {
    #[cfg(feature = "semantic")]
    {
        warm_semantic(args)
    }
    #[cfg(not(feature = "semantic"))]
    {
        let _ = args;
        bail!("ctx-mcp was built without semantic support; rebuild with --features semantic")
    }
}

pub(super) fn purge(args: ServeArgs) -> Result<()> {
    #[cfg(feature = "semantic")]
    {
        purge_semantic_cache(args)
    }
    #[cfg(not(feature = "semantic"))]
    {
        let _ = args;
        bail!("ctx-mcp was built without semantic support; rebuild with --features semantic")
    }
}

#[cfg(feature = "semantic")]
pub(super) fn warm_semantic(args: ServeArgs) -> Result<()> {
    let args = serve_args_with_default_root(args)?;
    ensure_semantic_enabled(&args, "warm")?;
    let started = Instant::now();
    println!("Warming context engine cache");
    for workspace in semantic_workspace_roots(&args)? {
        let provider = provider_for_roots(workspace.roots.clone(), scan_options(&args), &args)?;
        let snapshot = provider.snapshot().context("failed to scan workspace")?;
        let index = provider
            .semantic_index()
            .context("semantic index unavailable")?;
        let response = index.warm(&provider, &snapshot, &CancelToken::never())?;
        print_warm_response(&workspace, &response);
    }
    println!("elapsed: {:.1}s", started.elapsed().as_secs_f64());
    Ok(())
}

#[cfg(feature = "semantic")]
pub(super) fn purge_semantic_cache(args: ServeArgs) -> Result<()> {
    let args = serve_args_with_default_root(args)?;
    ensure_semantic_enabled(&args, "cache purge")?;
    println!("Purging context engine project cache");
    for workspace in semantic_workspace_roots(&args)? {
        let provider = provider_for_roots(workspace.roots.clone(), scan_options(&args), &args)?;
        let index = provider
            .semantic_index()
            .context("semantic index unavailable")?;
        match index.purge_cache()? {
            Some(dir) => println!("{}: purged {}", workspace.name, dir.display()),
            None => println!(
                "{}: no persistent semantic cache configured",
                workspace.name
            ),
        }
    }
    Ok(())
}

#[cfg(feature = "semantic")]
fn serve_args_with_default_root(mut args: ServeArgs) -> Result<ServeArgs> {
    if args.roots.is_empty() && args.workspaces.is_empty() {
        args.roots
            .push(std::env::current_dir().context("failed to read current directory")?);
    }
    Ok(args)
}

#[cfg(feature = "semantic")]
fn semantic_workspace_roots(args: &ServeArgs) -> Result<Vec<WorkspaceRoots>> {
    let mut workspaces = vec![WorkspaceRoots {
        name: "default".to_string(),
        roots: args.roots.clone(),
    }];
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
    workspaces.extend(
        grouped
            .into_iter()
            .map(|(name, roots)| WorkspaceRoots { name, roots }),
    );
    Ok(workspaces)
}

#[cfg(feature = "semantic")]
fn ensure_semantic_enabled(args: &ServeArgs, action: &str) -> Result<()> {
    if args.no_semantic {
        bail!("{action} requires semantic indexing; remove --no-semantic");
    }
    Ok(())
}

#[cfg(feature = "semantic")]
fn print_warm_response(workspace: &WorkspaceRoots, response: &SemanticWarmResponse) {
    println!("workspace: {}", workspace.name);
    println!("roots: {}", display_roots(&workspace.roots));
    println!("files in scope: {}", response.files_in_scope);
    println!("chunks: {}", response.chunks);
    if let Some(cache_dir) = &response.cache_dir {
        println!("cache: {}", cache_dir.display());
    }
    for diagnostic in &response.diagnostics {
        println!("warning: {}", diagnostic.message);
    }
}

#[cfg(feature = "semantic")]
fn display_roots(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
