# Crate Layout Architecture Notes

Date: 2026-06-17

## Goal

Keep public behavior stable while making the workspace easier to extend. The guiding rule is to make each module own one seam: CLI parsing, command execution, workspace construction, JSON-RPC serving, tool aggregation, authentication, provider integrations, repository-map ranking, and filesystem catalog scanning should evolve independently.

## Changes applied

### `crates/ctx-mcp/src`

`main.rs` is now a thin binary entrypoint. Runtime responsibilities moved behind explicit internal modules:

- `cli.rs` — Clap command model and top-level dispatch.
- `workspace.rs` — root/workspace argument types, semantic runtime config, provider/registry construction.
- `server.rs` — stdio JSON-RPC loop, initialization state, JSON-RPC response helpers.
- `tools.rs` — single aggregation seam for core tools plus xAI tools.
- `commands/` — user-facing command implementations:
  - `cache.rs`
  - `config.rs`
  - `doctor.rs`
  - `install.rs`
- `auth/commands.rs` — xAI OAuth CLI command surface, leaving token storage/refresh helpers in focused auth modules.
- `xai/tools/` — xAI API-domain handlers split by capability:
  - `models.rs`
  - `responses.rs`
  - `search.rs`
  - `image.rs`
  - `audio.rs`
  - `video.rs`

This makes new CLI commands additive under `commands/`, new tool providers additive through `tools.rs`, and xAI feature growth additive under `xai/tools/`.

### `crates/ctx-core/src`

Modules that already had child modules now use standard Rust directory modules:

- `build_context/mod.rs`
- `catalog/mod.rs`
- `semantic/index/mod.rs`
- `repomap/mod.rs`

Additional deepening splits applied:

- `catalog/fs_scan.rs` — filesystem walk, worker aggregation, diagnostics, deterministic snapshot finalization.
- `dispatch/editing/diff.rs` — pure diff rendering and diff options.
- `dispatch/tests/` — dispatch tests split by behavior area.
- `catalog/tests.rs` — filesystem catalog tests moved out of production module body.
- `repomap/` — PageRank repo-map split into focused internals:
  - `analysis.rs` — file parsing/indexing for repo-map and navigation reuse.
  - `graph.rs` — reference graph construction.
  - `imports.rs` — import-path resolution.
  - `language.rs` — language families and stopword/high-document-frequency filtering.
  - `query.rs` — query and seed normalization/matching.
  - `rank.rs` — PageRank, personalization, seed selection, score ordering.
  - `symbols.rs` — response symbol trimming.
  - `tests.rs` — ranking/import/language/cancellation coverage.

The public seams stay stable: `ctx_core::{get_repo_map, get_repo_map_cancellable, RepoMapRequest}` and internal navigation users still reach `crate::repomap::{IndexedFile, indexed_files_cancellable, resolve_import_reference}`.

## Current standard

- Source file size gate is hard and passes: every source file is within 600 non-test lines.
- Large modules are now deep rather than shallow: each extracted module hides real behavior behind a small parent-facing seam.
- `semantic/index` was inspected but not split further in this pass. It already has `search.rs` and persistence rebuild seams; the remaining lifecycle/build split is higher risk and should be done only with semantic-feature checks as a dedicated change.

## Remaining candidates, only if future work touches them

1. `ctx-core::semantic/index`
   - Candidate seam: background build orchestration, built-index construction, build lifecycle.
   - Risk: medium; touches generation/cancellation/cache semantics.

2. `ctx-core::dispatch/editing`
   - Candidate seam: result formatting, provider I/O, atomic/non-atomic mutation ops.
   - Risk: medium; selection rebasing behavior is subtle.

3. `ctx-core::catalog`
   - Candidate seam: codemap cache warming and FS provider trait implementation.
   - Risk: medium; invalidation and generation checks must remain exact.

4. `ctx-core::codemap` / `ctx-core::search`
   - Candidate seam: parser-specific AST helpers or ranking helpers.
   - Risk: medium; these are algorithmic deep modules and should not be split unless a concrete feature needs the seam.
