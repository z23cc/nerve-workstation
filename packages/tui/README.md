# Nerve Workstation TUI package

This package is the TypeScript frontend layer for the human-facing `nerve daemon` local Nerve Runtime protocol.
It is UI-neutral backend plumbing; actual TUI screens should use `NerveClient` rather than talking MCP directly.

## Current scope

- spawn `nerve daemon --stdio --root <path>`
- validate `runtime/info` against generated Rust protocol constants
- list runtime tools
- start/get/list/cancel async runtime jobs
- subscribe to `runtime/event` notifications

## Protocol source of truth

`nerve-runtime` owns the Rust protocol constants and types for the `nerve-runtime` protocol. Regenerate generated TypeScript after protocol changes:

```bash
bun run protocol:generate
```

## Smoke test

```bash
cargo build -p nerve-workstation --bin nerve
bun --cwd=packages/tui run smoke -- --root ../..
```
