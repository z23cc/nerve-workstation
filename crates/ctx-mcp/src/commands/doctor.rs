use anyhow::{Context, Result};
use std::process::Command;

pub(crate) fn doctor() -> Result<()> {
    println!("ctx-mcp doctor");
    print_command_version("rustc", ["--version"])?;
    print_command_version("cargo", ["--version"])?;
    println!("default features: codemap disabled (no C compiler required)");
    println!("status: ok");
    Ok(())
}

fn print_command_version<const N: usize>(cmd: &str, args: [&str; N]) -> Result<()> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("run {cmd}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = if stdout.trim().is_empty() {
        stderr.trim()
    } else {
        stdout.trim()
    };
    println!("{cmd}: {text}");
    Ok(())
}
