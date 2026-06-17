use anyhow::{Context, Result, bail};
use ctx_runtime::protocol_codegen::{CONSTANTS_PATH, SCHEMA_PATH, constants_json, schema_json};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() -> Result<()> {
    let check = env::args().skip(1).any(|arg| arg == "--check");
    let root = find_repo_root()?;
    let artifacts = [
        (SCHEMA_PATH, schema_json()),
        (CONSTANTS_PATH, constants_json()),
    ];
    if check {
        check_artifacts(&root, &artifacts)
    } else {
        write_artifacts(&root, &artifacts)
    }
}

fn find_repo_root() -> Result<PathBuf> {
    let mut dir = env::current_dir().context("current dir")?;
    loop {
        if dir.join("Cargo.toml").exists() && dir.join("crates/ctx-runtime").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("failed to locate repository root from current directory");
        }
    }
}

fn check_artifacts(root: &Path, artifacts: &[(&str, String)]) -> Result<()> {
    let mut stale = Vec::new();
    for (path, expected) in artifacts {
        let actual = fs::read_to_string(root.join(path)).unwrap_or_default();
        if actual != *expected {
            stale.push(*path);
        }
    }
    if stale.is_empty() {
        return Ok(());
    }
    bail!(
        "runtime protocol Rust artifacts are stale: {}. Run `cargo run -p ctx-runtime --bin export-runtime-protocol`",
        stale.join(", ")
    )
}

fn write_artifacts(root: &Path, artifacts: &[(&str, String)]) -> Result<()> {
    for (path, content) in artifacts {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}
