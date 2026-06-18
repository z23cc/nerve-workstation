# Architecture North Star

Status: **governing** — read before any structural change. Referenced as a binding rule from `CLAUDE.md`.
Date: 2026-06-18

This is the long-term architectural contract for Nerve Workstation. It exists so that every
incremental change is **locally optimal _and_ globally aligned**: each feature plugs into a declared
seam instead of bolting on a new bespoke entry point. When in doubt, the seam wins over the shortcut.

## 1. North star

> **Nerve = a deterministic code-intelligence kernel + a thin protocol-defined runtime +
> "everything else is a plugin behind a port."**

- The **kernel** (`nerve-core`) is pure and deterministic; it is golden-tested and is never extended
  by runtime plugins.
- Non-determinism (LLMs, third-party tools, sessions, network, time) lives strictly **above** the
  kernel.
- Extending the system means **implementing a declared seam** — never editing the kernel and never
  opening a new ad-hoc host entry point.

## 2. Prime directive (local-optimal = global-optimal)

> **Every new capability MUST enter through a declared seam (port / registry / protocol). Never open
> a bespoke entry point.**

Cautionary tale: `nerve agent run` was added as a synchronous CLI path that bypassed
`RuntimeCommand` / job / event. It worked locally but created a third, off-protocol "face" and broke
the protocol-authority invariant (§3.3). **A capability without a seam is guaranteed debt.** It is on
the roadmap (P0) to be folded back in as a `RuntimeCommand`.

## 3. Invariants (do not break)

1. **Determinism boundary.** `nerve-core` is deterministic and golden-tested. Nothing
   non-deterministic (LLM calls, network, wall-clock, third-party plugins) may enter `nerve-core`;
   it all lives in `nerve-runtime` / `nerve-agent` / `nerve-workstation`. Golden snapshots
   (`crates/nerve-core/tests/snapshots/*.snap`) guard this.
2. **Runtime is the single dispatch hub.** All tool execution goes through `Runtime`
   (`handle_tool_call*` / `handle_command*`). No host (MCP, daemon, agent, CLI) may call
   `nerve-core` dispatch directly.
3. **Runtime types are the single protocol authority.** The human-facing runtime protocol vocabulary
   (`RuntimeCommand`, `RuntimeEvent`, `Runtime*Request`, method-name constants) is defined **only** in
   `nerve-runtime`, codegen'd to TS via `bun run protocol:generate`, and drift-checked in CI
   (`bun run protocol:check` + the `generated_protocol_rust_artifacts_are_current` test). Protocol
   changes are **additive and versioned** — never break a published field.
4. **Protocol types are transport-neutral data.** Commands/events carry plain serde/JSON
   (e.g. `tool.call` = `{name, arguments: Value}`), never references to engine/agent domain types.
   Consequently `nerve-runtime` depends **only** on `nerve-core` — never on `nerve-agent`. The
   composition root translates protocol data ⇄ domain types.
5. **MCP is a separate, external protocol.** The MCP face (`server.rs`, pinned `2024-11-05`) consumes
   the `Runtime` dispatch hub but owns its own wire vocabulary (the MCP standard). It is **not** under
   the runtime-protocol authority. Do not conflate the two; never put session/agent vocabulary into
   the MCP face.
6. **Composition only in the binary.** `nerve-workstation` is the sole composition root (wires
   adapters, toolbox, session manager, persistence). Sibling crates `nerve-runtime` and `nerve-agent`
   never depend on each other.

## 4. Crates & layers (current)

```
nerve-core       deterministic kernel — CatalogProvider port → immutable CatalogSnapshot;
  ▲   ▲          tools (search/read/tree/codemap/repomap/navigate/edit/semantic/build_context);
  │   │          dispatch hub entry (handle_tool_call*). Golden-tested. Depends on nothing internal.
  │   │
  │   └── nerve-agent   LLM layer — LlmProvider trait + Anthropic/OpenAI-Responses/xAI adapters,
  │                     multi-provider OAuth + credential store, Orchestrator loop, ToolBox port.
  │                     Depends only on nerve-core (for CancelToken). Runtime/protocol-agnostic.
  └────── nerve-runtime  protocol authority + dispatch hub wrapper + RuntimeToolAdapter registry +
                         job/event protocol (v3). Depends only on nerve-core.
                              ▲
nerve-workstation   composition root (the `nerve` binary): MCP face (server.rs), daemon face
  ▲                 (daemon/, jobs.rs), CLI (cli.rs), agent wiring (agent.rs = RuntimeToolBox),
  │                 xAI tools (xai/), thin `nerve auth` alias (auth/ → nerve-agent::auth).
  │
packages/tui (+ future GUI/mobile)   clients of the versioned runtime protocol — never the engine.
```

`nerve-agent` and `nerve-runtime` are **siblings** (both depend only on `nerve-core`); the binary
marries them via the `ToolBox` port (`RuntimeToolBox` in `agent.rs`).

## 5. Seam scorecard

Most plugin seams **already exist as Rust ports** — the work is to promote them to first-class,
registry/config-driven extension points and to add the missing layers (marked ✗).

| Seam (port) | Defined in | Today | Gap to "plugin-grade" |
|---|---|---|---|
| `CatalogProvider` | `nerve-core/port.rs` | Fs / Memory | compile-time; fine (could add Git / remote overlay) |
| `RuntimeToolAdapter` | `nerve-runtime` | xAI only | **add registry + an MCP-client adapter** (highest leverage) |
| `LlmProvider` | `nerve-agent/provider` | 3 hard-coded | add registry + **config-driven** (OpenAI-compatible = config only) |
| `ToolBox` | `nerve-agent/provider` | `RuntimeToolBox` | fine (agent↔tools seam is wired) |
| `AuthStrategy` | `nerve-agent/auth` | 3 providers | fine; could be config-driven |
| Session / Conversation | — | ✗ none | **missing protocol layer** (super-app prerequisite) |
| Skill / AgentDef | — | `AgentDef` exists, no loader | ✗ **capabilities-as-data** (no recompile) |
| Policy / Permission | — | ✗ none | ✗ **prerequisite for safe plugins** |
| Hooks | — | orchestrator has lifecycle shape | ✗ interception points |
| Persistence | — | jobs are in-memory | ✗ conversations / credentials / plugin config |

## 6. Plugin architecture — layered by audience

Do not build one plugin system; layer by what is being extended, each with the right mechanism:

1. **Tools — dual track.**
   - **MCP client (highest leverage, reuses an existing port):** an `McpClientToolAdapter:
     RuntimeToolAdapter` that consumes external MCP servers — their specs flow into `tool_specs()`
     and calls proxy through. Any MCP server becomes a nerve tool with **zero recompile**, via the
     industry standard. (nerve is already an MCP server; being a client is symmetric.)
   - **First-party `RuntimeToolAdapter`** (compile-time, zero overhead): e.g. xAI.
2. **Model providers — trait + config.** `LlmProvider` for non-compatible wire formats; **config**
   for the OpenAI-compatible long tail (`{base_url, api_key, wire}` — no code).
3. **Capabilities — data as plugin.** Skills (markdown + optional scripts, discovered from
   directories) and Agent/Workflow definitions (YAML; precedence project > global > built-in).
   No recompile; versioned.
4. **Lifecycle — hooks.** Expose the orchestrator's Start/Request/Response/End lifecycle as hook
   points (memory, compaction, telemetry, policy).
5. **Data sources — `CatalogProvider`** (already a port).

> Key insight: the only genuinely *new* mechanisms needed are (1) MCP-client, (2) skill/agent-def
> loader, (3) the session layer, (4) the permission engine, (5) persistence. Everything else is
> promoting existing ports to registries/config.

## 7. Stable contracts to lock (so future work is additive, not breaking)

1. **Versioned runtime protocol** (`nerve-runtime`) — session/agent vocabulary added as data (v3→v4);
   never break published fields. Codegen + drift-test enforced.
2. **Provider config schema** — adding a provider = adding config.
3. **Tool / MCP registry + spec contract** — discovery, namespacing, dedup (`Runtime` already dedups).
4. **Session / Conversation model** — the missing protocol layer.
5. **Skill / AgentDef format** — versioned data contract.
6. **Persistence schema** — conversations / credentials / plugin config, migratable.
7. **Permission model** — capability declaration + authorization decision.
8. **Extract a thin `nerve-protocol` crate** when third-party Rust plugins/clients appear, so they
   depend on protocol types only, not all of `nerve-runtime`.

## 8. Roadmap (each step locally + globally optimal)

- **P0 — Session layer (folds in the off-protocol agent).** Add a session/agent command family to
  `nerve-runtime` (as data) + structured agent events; run the orchestrator as a daemon job. Restores
  invariant §3, unlocks GUI/TUI driving the agent. **Prerequisite for everything.**
- **P1 — MCP client.** Tools-as-plugins via an `McpClientToolAdapter` (reuses `RuntimeToolAdapter`).
  Highest ROI, near-zero new architecture.
- **P2 — Provider registry + config-driven providers.**
- **P3 — Skills + Agent/Workflow definitions** (capabilities-as-data, with a loader).
- **P4 — Permission / policy engine** (prerequisite for safely enabling third-party plugins).
- **P5 — Persistence + migrations** (session resume, multi-session, plugin config).
- **P6 — Hooks + GUI (Tauri) / TUI / mobile** (daemon WS transport reuses the transport-neutral router).

## 9. Risks & anti-goals

- **Determinism is non-negotiable.** Plugins (MCP / skills / providers) live above the kernel; never
  let one touch `nerve-core` — it would destroy golden-test reproducibility.
- **Security before openness.** Ship the permission engine + trust gates (P4) **before** enabling
  third-party MCP servers or script-bearing skills. A plugin is arbitrary code execution.
- **Versioned or dead.** Once a protocol / provider / skill contract ships, it is additive-only.
- **Anti-goals:** no premature WASM plugin host; no bespoke plugin protocol (MCP is the standard);
  no kitchen-sink protocol (a session is not strings stuffed into `JobProgress`); no premature crate
  splitting (split only when independent versioning is needed, e.g. `nerve-protocol`).

## 10. Governance — how the invariants stay true

- **Enforced by CI today:** protocol drift (`protocol:check` + `generated_protocol_rust_artifacts_are_current`),
  determinism (golden snapshots), file/function size, `clippy -D warnings`, fmt.
- **To add:** a test/lint that fails if a host executes work outside `Runtime` (closes the
  "convention-only" gap that let `agent run` slip the protocol).
- **Per seam:** a registry + contract tests (adapter name dedup, spec shape, provider config
  validation).
- **Record contract evolution** alongside `docs/parity/` (the differential ledger style).

When this document and the code disagree, treat it as a bug in one of them: either the change skipped
a seam (fix the change) or the seam genuinely evolved (update this doc in the same PR).
