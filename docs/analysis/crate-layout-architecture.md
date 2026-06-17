# Crate Layout Architecture Notes

Date: 2026-06-17

## Goal

Keep public behavior stable while making the workspace easier to extend. The guiding rule is to make each module own one seam: CLI parsing, command execution, workspace construction, JSON-RPC serving, tool aggregation, authentication, provider integrations, repository-map ranking, and filesystem catalog scanning should evolve independently.

## Changes applied

### `crates/ctx-runtime/src`

`ctx-runtime` is the transport-neutral runtime seam above `ctx-core`. It owns capability composition without owning any transport protocol:

- `Runtime<R>` — owns a `WorkspaceResolver` and dispatches tool calls.
- `RuntimeToolAdapter<R>` — adapter seam for provider-specific or host-specific capabilities.
- `RuntimeError` — preserves core dispatch errors and adapter messages for transports to render.
- `RuntimeCommand` / `RuntimeEvent` — human-facing job command/event contracts for daemon and TUI hosts.
- `RuntimeJob*` / `RuntimeInfo` / `RuntimeToolSpec` types — protocol v3 job start/get/list/cancel request, runtime metadata, and tool schema shared by the daemon and TypeScript backend.
- `protocol_codegen.rs` plus `export-runtime-protocol` — generates Rust-owned protocol schema/constants under `docs/protocol/`; `packages/tui/scripts/generate-protocol.ts` feeds that schema into `json-schema-to-typescript` so Rust protocol schema stays the source of truth for TypeScript clients without a custom TS compiler.

Runtime dispatch order is explicit: registered adapters are consulted first, then the built-in `ctx-core` dispatcher handles unclaimed tools. Tool specs are de-duplicated by name with core specs kept first, so accidental adapter duplicates do not leak ambiguous tool definitions. This keeps MCP, CLI, TUI, and future daemon/Web hosts from each re-implementing tool aggregation.

### `packages/tui/src`

The TypeScript frontend package owns the human-facing backend adapter:

- `backend/CtxDaemonClient` — spawns `ctx-mcp daemon --stdio`, uses protocol v3 job methods, and dispatches `runtime/event` notifications.
- `backend/protocol.generated.ts` — generated protocol constants and TypeScript types from Rust schema.
- `backend/types.ts` — UI-neutral `WorkstationBackend` plus frontend aliases over generated protocol types.
- `cli/smoke.ts` — local integration smoke check that starts, polls, and lists a job through the daemon protocol.

This keeps UI components independent from MCP and from Rust process details.

### `crates/ctx-mcp/src`

`main.rs` is now a thin binary entrypoint. Runtime responsibilities moved behind explicit internal modules:

- `cli.rs` — Clap command model and top-level dispatch.
- `workspace.rs` — root/workspace argument types, semantic runtime config, provider/registry construction.
- `server.rs` — MCP stdio JSON-RPC loop and initialization state.
- `daemon.rs` — human-facing runtime JSON-RPC/NDJSON daemon over stdio; routes protocol v3 job methods.
- `daemon/tests.rs` — daemon protocol tests kept out-of-line so production file size stays below the hard cap.
- `jobs.rs` — daemon-owned in-memory runtime job lifecycle, cancellation tokens, retention, and event emission.
- `rpc.rs` — shared JSON-RPC response and line-writing helpers.
- `tools.rs` — MCP crate runtime assembly; registers the xAI tool adapter with `ctx-runtime`.
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

This makes new CLI commands additive under `commands/`, new tool providers additive as runtime adapters, and xAI feature growth additive under `xai/tools/`.

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

- `ctx-runtime` is the stable seam for Core Runtime + multiple Adapter architecture; MCP and daemon are consumers of runtime, not the core architecture.
- `ctx-mcp daemon --stdio` exposes protocol v3 for TUI/frontends: JSON-RPC 2.0 over NDJSON stdio, `runtime/event` notifications, and `runtime/jobs/start|get|list|cancel`.
- `ctx-mcp serve` remains the agent-facing MCP protocol and is separate from the human-facing daemon protocol.
- `packages/tui` provides the first TypeScript `WorkstationBackend` adapter over that daemon protocol.
- Clients execute runtime commands through the daemon job lifecycle only; the daemon does not expose the old synchronous `runtime/command` method.
- Job progress events are coarse today; core tools cooperatively observe cancellation tokens, but the protocol does not promise detailed percentages for every operation.
- Source file size gate is hard and passes: every source file is within 600 non-test lines.
- Large modules are now deep rather than shallow: each extracted module hides real behavior behind a small parent-facing seam.
- `semantic/index` was inspected but not split further in this pass. It already has `search.rs` and persistence rebuild seams; the remaining lifecycle/build split is higher risk and should be done only with semantic-feature checks as a dedicated change.

## Remaining candidates, only if future work touches them

1. `ctx-mcp::auth` / `ctx-mcp::commands::cache`
   - Candidate seam: move auth status/login/logout and cache warm/purge execution behind typed runtime commands/events.
   - Risk: medium; browser OAuth and persistent auth storage must preserve current behavior.

2. TypeScript TUI component tree
   - Candidate seam: build UI screens on top of `WorkstationBackend`, not directly on process/MCP calls.
   - Risk: low-medium; UI state should not leak into the Rust runtime protocol.

3. `ctx-core::semantic/index`
   - Candidate seam: background build orchestration, built-index construction, build lifecycle.
   - Risk: medium; touches generation/cancellation/cache semantics.

4. `ctx-core::dispatch/editing`
   - Candidate seam: result formatting, provider I/O, atomic/non-atomic mutation ops.
   - Risk: medium; selection rebasing behavior is subtle.

5. `ctx-core::catalog`
   - Candidate seam: codemap cache warming and FS provider trait implementation.
   - Risk: medium; invalidation and generation checks must remain exact.

6. `ctx-core::codemap` / `ctx-core::search`
   - Candidate seam: parser-specific AST helpers or ranking helpers.
   - Risk: medium; these are algorithmic deep modules and should not be split unless a concrete feature needs the seam.
