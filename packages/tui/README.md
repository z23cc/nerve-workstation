# ctx-mcp TUI package

This package is the TypeScript frontend layer for the human-facing `ctxd` local AI Workstation Runtime protocol.
It is UI-neutral backend plumbing; actual TUI screens should use the preferred `CtxdClient`
export without binding component code to MCP or Rust process details.

This package is managed by the repository root Bun workspace (`bun@1.3.14`). Protocol types in `src/backend/protocol.generated.ts` are generated from Rust schema via `json-schema-to-typescript` and should not be edited by hand.

Regenerate protocol artifacts from the repository root with:

```bash
bun run protocol:generate
```

Current scope:

- spawn `ctx-mcp ctxd --stdio --root <path>`
- speak protocol v3: JSON-RPC 2.0 over NDJSON stdio
- read `runtime/info` and `runtime/tools/list`
- run jobs through `runtime/jobs/start|get|list|cancel`
- consume `runtime/event` notifications for job lifecycle/progress events
- use `runJob()` only as a convenience wrapper over the job API

`ctx-runtime` owns the runtime protocol constants and types. `ctxd` provides the local runtime-daemon
transport for frontend clients; MCP remains a separate adapter for agent-facing hosts.

The frontend layer intentionally does not call or model legacy synchronous runtime commands.
Long-running tool calls should be listed and cooperatively cancelled through the job API.

Run the smoke check from the repository root after building the Rust binary:

```bash
cargo build -p ctx-mcp
bun run tui:smoke
```

The smoke command starts a `ping` job, polls it through `runtime/jobs/get`, verifies it appears in
`runtime/jobs/list`, and prints the observed `runtime/event` notifications.
