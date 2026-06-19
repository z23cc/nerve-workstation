import { test } from "bun:test";
import assert from "node:assert/strict";
import { formatAgentEvent, parseArgs, preview } from "../src/cli/chat.ts";

const strip = (s: string): string => s.replace(/\x1b\[[0-9;]*m/g, "");

test("assistant message renders as raw streamed text (no decoration)", () => {
  assert.equal(formatAgentEvent({ kind: "message", text: "hello world" }), "hello world");
});

test("reasoning renders dimmed", () => {
  assert.equal(strip(formatAgentEvent({ kind: "reasoning", text: "thinking" })), "thinking");
});

test("tool_started shows the tool name and an args preview", () => {
  const out = strip(formatAgentEvent({ kind: "tool_started", tool: "read_file", arguments: { path: "a.rs" } }));
  assert.match(out, /→ read_file/);
  assert.match(out, /path/);
});

test("tool_finished distinguishes ok from failure and shows failure output", () => {
  assert.match(strip(formatAgentEvent({ kind: "tool_finished", tool: "edit", ok: true, output: "" })), /✓ edit/);
  const failed = strip(formatAgentEvent({ kind: "tool_finished", tool: "edit", ok: false, output: "boom" }));
  assert.match(failed, /✗ edit/);
  assert.match(failed, /boom/);
});

test("interrupted shows the reason", () => {
  assert.match(strip(formatAgentEvent({ kind: "interrupted", reason: "cancelled" })), /interrupted: cancelled/);
});

test("turn_started renders nothing inline", () => {
  assert.equal(formatAgentEvent({ kind: "turn_started", turn: 1 }), "");
});

test("preview collapses whitespace and truncates", () => {
  assert.equal(preview({ a: 1 }), '{"a":1}');
  assert.equal(preview("x".repeat(200)).length, 80);
  assert.match(preview("a\n  b\tc"), /a b c/);
});

test("parseArgs reads provider/model and defaults root to cwd", () => {
  const args = parseArgs(["--provider", "anthropic", "--model", "claude-sonnet-4"]);
  assert.equal(args.provider, "anthropic");
  assert.equal(args.model, "claude-sonnet-4");
  assert.equal(args.root, process.cwd());
});
