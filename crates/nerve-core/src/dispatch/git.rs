use super::{DispatchError, GitArgs, edit};
use std::path::Path;

/// Run a read-only git subcommand in `root` and return its stdout (capped).
pub(super) fn run_git(root: &Path, args: &GitArgs) -> Result<String, DispatchError> {
    let bad = |detail: String| {
        DispatchError::Edit(edit::EditError::Parse {
            mode: "git",
            detail,
        })
    };
    if let Some(path) = &args.path
        && path.split(['/', '\\']).any(|segment| segment == "..")
    {
        return Err(bad("path traversal is not allowed".to_string()));
    }
    let mut git: Vec<String> = Vec::new();
    match args.op.as_str() {
        "status" => git.extend(["status", "--short", "--branch"].map(String::from)),
        "diff" => {
            git.push("diff".to_string());
            if args.staged {
                git.push("--staged".to_string());
            }
            if let Some(path) = &args.path {
                git.push("--".to_string());
                git.push(path.clone());
            }
        }
        "log" => {
            git.extend(["log", "--oneline"].map(String::from));
            git.push("-n".to_string());
            git.push(args.count.to_string());
            if let Some(path) = &args.path {
                git.push("--".to_string());
                git.push(path.clone());
            }
        }
        "blame" => {
            let path = args
                .path
                .as_ref()
                .ok_or_else(|| bad("blame requires a path".to_string()))?;
            git.push("blame".to_string());
            if let Some(lines) = &args.lines {
                git.push("-L".to_string());
                git.push(lines.clone());
            }
            git.push("--".to_string());
            git.push(path.clone());
        }
        "show" => {
            let reference = args
                .reference
                .as_ref()
                .ok_or_else(|| bad("show requires a ref".to_string()))?;
            git.push("show".to_string());
            git.push(reference.clone());
        }
        other => return Err(bad(format!("unknown git op: {other}"))),
    }
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(&git)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|err| bad(format!("could not run git: {err}")))?;
    if !output.status.success() {
        return Err(bad(format!(
            "git {} failed: {}",
            args.op,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    if text.chars().count() > 20_000 {
        let capped: String = text.chars().take(20_000).collect();
        Ok(format!("{capped}\n\u{2026} (output truncated)\n"))
    } else {
        Ok(text.into_owned())
    }
}
