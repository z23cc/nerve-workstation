use crate::workspace::ServeArgs;
use anyhow::{Context, Result};
use nerve_core::RootPolicy;

pub(crate) fn config_roots(args: ServeArgs) -> Result<()> {
    let policy = RootPolicy::new(args.roots).context("invalid root policy")?;
    if policy.roots().is_empty() {
        println!("roots: []");
        println!("fail_closed: true");
        return Ok(());
    }
    for root in policy.roots() {
        println!("{}\t{}", root.id, root.path.display());
    }
    Ok(())
}
