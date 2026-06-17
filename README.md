# context-engine-rs

A deterministic **code-intelligence engine** exposed as an MCP server over stdio
and `ctxd`, a local AI Workstation Runtime for frontends. One pure-Rust binary gives MCP hosts (Claude Code,
Codex, …) and runtime clients fast
search, codemaps, symbol navigation, structural edits, and semantic retrieval over
a codebase — no language server or GUI required.

## Highlights

- **27 MCP tools**: search, read, tree, codemap, repo-map, symbol nav, call
  hierarchy, structural AST search/rewrite, a 4-mode edit engine, read-only git,
  semantic search, context assembly, plus optional xAI/Grok tools when OAuth is configured.
- **Codemap over 11 languages** (tree-sitter): signatures **with return types**,
  struct/class **fields**, and full nested symbols — Rust, Python, JS, TS/TSX, Go,
  Java, C, C++, C#, Ruby, PHP.
- **Deterministic** by design: snapshot-centered, golden-tested, reproducible
  (the lexical/structural tools give the same output for the same input).
- **Hybrid search**: ripgrep-style path/content (BM25) **plus** a built-in
  semantic engine — local ONNX embeddings + ANN + cross-encoder rerank, fused via
  RRF (on by default; see below).
- **Symbol navigation** (`goto_definition` / `find_references` / `call_hierarchy`)
  with confidence scoring — the structured layer agentic coders otherwise lack.
- **Cross-platform single binary**: Homebrew bottle, Scoop, or `cargo install`.

## Install

```bash
# macOS / Linux
brew install z23cc/tap/ctx-mcp

# Windows
scoop bucket add z23cc https://github.com/z23cc/scoop-bucket && scoop install ctx-mcp

# From source
cargo install --path crates/ctx-mcp
```

macOS pours a prebuilt **bottle** (instant); other platforms build from source.
See [`packaging/homebrew`](packaging/homebrew/README.md) for how bottles/releases work.

## Use with Claude Code / Codex (MCP)

One command registers `ctx-mcp` (idempotent, writes an absolute `--root`):

```bash
ctx-mcp install            # both; root = current dir   (--claude / --codex / --dry-run)
ctx-mcp warm               # optional: prebuild the current project's semantic index
ctx-mcp cache purge        # delete the current project's semantic index cache
ctx-mcp auth login xai     # optional: browser OAuth for xAI Grok subscription access
ctx-mcp auth status        # show xAI OAuth status without printing secrets
ctx-mcp auth logout        # remove stored xAI OAuth credentials
```

Or configure manually — **Claude Code** (`.mcp.json`) / **Codex** (`~/.codex/config.toml`):

```json
{ "mcpServers": { "context-engine": {
  "command": "ctx-mcp", "args": ["serve", "--root", "/abs/path/to/project"] } } }
```
```toml
[mcp_servers.context-engine]
command = "ctx-mcp"
args = ["serve", "--root", "/abs/path/to/project"]
```

The stdio loop pins MCP `protocolVersion` `2024-11-05` and is **fail-closed**: no
`--root` means catalog/read/search are refused. Numeric params also accept
integer-valued strings (e.g. `"limit": "120"`).

## `ctxd` local AI Workstation Runtime

Human-facing frontends should use `ctxd`, the local AI Workstation Runtime, instead of MCP:

```bash
ctx-mcp ctxd --stdio --root /abs/path/to/project
```


`ctx-runtime` is the protocol source of truth. MCP stdio is an agent-facing adapter,
the runtime daemon stdio path is a transport adapter, and the TypeScript TUI backend
is a client of the runtime protocol.

Protocol v3 is a small JSON-RPC 2.0 subset over newline-delimited JSON (NDJSON)
on stdio. Each stdin line is one request object; each stdout line is one response
or notification. The daemon keeps method routing separate from stdio transport so
future UDS or Named Pipe transports can reuse the same runtime router. Event
notifications use method `runtime/event`. Requests with an `id` receive a response;
notifications without `id` do not.

Stable methods:

- `runtime/info` — protocol v3 metadata and capabilities.
- `runtime/tools/list` — runtime-visible tools.
- `runtime/jobs/start` — start `{ "job_id"?: string, "command": RuntimeCommand }`.
- `runtime/jobs/get` — get `{ "job_id": string, "include_result"?: boolean }`.
- `runtime/jobs/list` — list jobs with `{ "include_terminal"?: boolean, "include_results"?: boolean, "limit"?: number }`.
- `runtime/jobs/cancel` — cooperatively cancel `{ "job_id": string }`.

`RuntimeCommand` is `{ "kind": "ping" }`, `{ "kind": "tool.list" }`, or
`{ "kind": "tool.call", "name": string, "arguments"?: object }`. These kinds
are advertised as `capabilities.jobs.commandKinds`. Clients must execute commands
through `runtime/jobs/*`; job state is in-memory and disappears
when the daemon exits. Job events are `job_started`, coarse `job_progress`,
`job_cancel_requested`, and terminal `job_completed` / `job_failed` /
`job_cancelled`. Cancellation is cooperative; core tools check cancellation, while
some adapter or network calls may only stop after the current operation returns.

Example job request:

```json
{"jsonrpc":"2.0","id":1,"method":"runtime/jobs/start","params":{"job_id":"ping-1","command":{"kind":"ping"}}}
```

Example output (timestamps shortened; background progress/terminal events can interleave with the start response after `job_started`):

```json
{"jsonrpc":"2.0","method":"runtime/event","params":{"command":"ping","job_id":"ping-1","tool_name":null,"type":"job_started"}}
{"jsonrpc":"2.0","id":1,"result":{"job":{"job_id":"ping-1","status":"running","command":"ping","tool_name":null,"created_at_ms":0,"started_at_ms":0,"updated_at_ms":0,"finished_at_ms":null,"cancel_requested":false,"result":null,"error":null}}}
{"jsonrpc":"2.0","method":"runtime/event","params":{"current":null,"job_id":"ping-1","message":"executing runtime command","stage":"executing","total":null,"type":"job_progress"}}
{"jsonrpc":"2.0","method":"runtime/event","params":{"job_id":"ping-1","type":"job_completed"}}
```

## xAI Grok OAuth

`ctx-mcp` can store xAI Grok OAuth credentials for integrations that want to
reuse a SuperGrok / X Premium+ browser subscription path instead of an API key:

```bash
ctx-mcp auth login xai          # opens the xAI browser OAuth PKCE flow
ctx-mcp auth login xai --force  # discard reuse and start a fresh login
ctx-mcp auth status --refresh   # refresh if expiring, then print status
ctx-mcp auth logout             # non-interactive removal
```

Tokens are stored under the platform config directory by default (for example
`~/Library/Application Support/ctx-mcp/auth.json` on macOS or
`$XDG_CONFIG_HOME/ctx-mcp/auth.json` on Linux). If an existing legacy
`~/.ctx-mcp/auth.json` is present, ctx-mcp keeps using it. Set `CTX_MCP_HOME` or
`CTX_MCP_AUTH_FILE` to override the location. Tokens are stored in the OS
keychain when available, with a private-file JSON fallback. The stored bearer is
only sent to `https://api.x.ai/v1` or another `https://*.x.ai` URL.

The MCP server always lists Grok-backed tools, but they require `ctx-mcp auth
login xai` before use: `xai_models`, `xai_responses`, `x_search` (preferred X
search), `xai_x_search` (explicit alias), `web_search` (preferred generic web
search), `xai_web_search` (explicit alias), `xai_image_generate`, `xai_tts`,
`xai_transcribe`, and `xai_video_generate`.
Media generation tools require an explicit workspace-gated `output_path` so large
binary data is written to disk instead of returned inline.
A 403 from xAI usually means the signed-in account does not have the required
Grok/API entitlement.

## Tools

| Group | Tools |
|---|---|
| Search / read | `file_search` (path+content, BM25, smart-case, glob `include`/`exclude`/`extensions`, `output_mode`, asymmetric context; per-file cap + round-robin so one file can't monopolize results), `read_file` (line ranges, hashline view, **structural `summary` view** — signatures kept, bodies elided, with concrete re-read ranges), `get_file_tree` (budgeted ASCII tree) |
| Code intelligence | `get_code_structure` (codemap + signatures/fields + per-file `token_count`), `get_repo_map` (deterministic PageRank), `goto_definition` / `find_references` (confidence-scored) / `call_hierarchy` |
| Semantic | `semantic_search` (hybrid dense + BM25 + rerank; structured `index_state` = `ready`/`warming`/`bm25_only` + snapshot `generation` for freshness) |
| Edit | `edit` (`replace`/`patch`/`apply_patch`/`hashline`) / `write` / `delete` / `move` — root-gated, with unified diff (configurable context, optional ignore-whitespace) + syntax diagnostics; `ast_search` / `ast_edit` (structural — raw tree-sitter `query` mode **plus a `$META` pattern mode**) |
| Context / ops | `manage_selection`, `workspace_context`, `build_context`, `git` (read-only), `manage_workspaces` |
| xAI / Grok | `xai_models`, `xai_responses`, `x_search` (preferred), `xai_x_search`, `web_search` (preferred), `xai_web_search`, `xai_image_generate`, `xai_tts`, `xai_transcribe`, `xai_video_generate` |

## Semantic search (built in, on by default)

`semantic_search` works out of the box. The first call returns **BM25 results
immediately** while the dense index warms in the background, then auto-upgrades to
full hybrid (embeddings + ANN + rerank).

- **Model**: downloaded once **per machine** to `~/Library/Caches/context-engine-rs`
  (`~/.cache/...` on Linux), shared across all projects — never re-downloaded per
  directory. ~300 MB on first semantic use.
- **Index**: persisted **per project** (keyed by canonical roots + model + scope +
  version), so projects stay isolated and a warm index loads instantly.
- **Opt out**: `serve --no-semantic`. If you never call `semantic_search`, nothing
  is downloaded or built.
- **Tune**: `--semantic-cache-dir`, `--semantic-model-cache-dir`, `--semantic-no-rerank`,
  and scope flags (`--semantic-include` / `--semantic-exclude` / `--semantic-extension`).
- `build_context` automatically folds semantic candidates into its ranking when a
  warm index is available, then does a deterministic 1-hop type-reference expansion:
  files defining symbols the seed files reference are pulled in as codemap-only context.
- Structural summaries (`read_file view="summary"`) are memoized by content hash + fold options.

`build_context` / `read_file` etc. fall back gracefully when the index is cold.

## Build & quality gates

Frontend workspace scripts use Bun (`packageManager: bun@1.3.14`). Runtime protocol TypeScript types are generated from Rust schema.

```bash
cargo build
cargo test                                    # add --features semantic for the engine
cargo clippy --all-targets -- -D warnings     # functions <=100 lines, nesting capped
cargo fmt --check
./Scripts/check-file-size.sh                  # files <=600 non-test lines
bun run protocol:check

# Regenerate protocol schema/constants + TypeScript types after changing ctx-runtime protocol types
bun run protocol:generate

# Optional frontend adapter smoke check
cargo build -p ctx-mcp
bun run tui:smoke
```

Building requires a C toolchain (tree-sitter grammars compile `parser.c`).
Conventions: [`docs/CONVENTIONS.md`](docs/CONVENTIONS.md).

## Internals & design

- **Layout**: `crates/ctx-core` (engine + tools), `crates/ctx-runtime`
  (transport-neutral runtime protocol + tool-adapter composition), `crates/ctx-mcp` (stdio
  MCP adapter + CLI: `serve`/`ctxd`/`doctor`/`config`/`install`), `packages/tui`
  (TypeScript frontend backend adapter/client for `ctxd`).
- **Snapshot-centered**: filesystem access is behind a `CatalogProvider` port;
  the core operates on immutable `CatalogSnapshot` values. Codemap parses cache by
  `(mtime, size)`.
- **Determinism / parity**: golden snapshots under `crates/ctx-core/tests`; the
  RepoPrompt difference ledger lives in [`docs/parity/`](docs/parity/).
- **Plans**: see [`docs/plans/`](docs/plans/).
