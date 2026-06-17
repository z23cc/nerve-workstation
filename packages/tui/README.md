# ctx-mcp TUI package

This package is the TypeScript frontend layer for the human-facing `ctx-mcp daemon` protocol.
It is UI-neutral backend plumbing; actual TUI screens can build on `CtxDaemonClient` without
binding component code to MCP or Rust process details.

Current scope:

- spawn `ctx-mcp daemon --stdio --root <path>`
- speak protocol v2: JSON-RPC 2.0 over NDJSON stdio
- read `runtime/info` and `runtime/tools/list`
- run job-backed commands through `runtime/jobs/start|get|list|cancel`
- consume `runtime/event` notifications for job lifecycle/progress events
- keep `runCommand()` as a convenience wrapper over the job API

The Rust daemon still supports legacy synchronous `runtime/command`, but new frontend code should
prefer the job API so long-running tool calls can be listed and cooperatively cancelled.

Run the smoke check from the repository root after building the Rust binary:

```bash
cargo build -p ctx-mcp
cd packages/tui
npm run smoke -- --root ../..
```

The smoke command starts a `ping` job, polls it through `runtime/jobs/get`, verifies it appears in
`runtime/jobs/list`, and prints the observed `runtime/event` notifications.
