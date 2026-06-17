import { test } from "bun:test";
import assert from "node:assert/strict";
import { CtxDaemonClient } from "../src/backend/ctxDaemonClient.ts";

function notStartedClient(): CtxDaemonClient {
  return new CtxDaemonClient({ root: process.cwd(), binary: "ctx-mcp" });
}

test("client reports not started before start", async () => {
  const client = notStartedClient();
  await assert.rejects(() => client.info(), /not started/);
});

test("job API reports not started before start", async () => {
  const client = notStartedClient();
  await assert.rejects(() => client.startJob({ kind: "ping" }), /not started/);
  await assert.rejects(() => client.getJob("job-1"), /not started/);
  await assert.rejects(() => client.listJobs(), /not started/);
  await assert.rejects(() => client.cancelJob("job-1"), /not started/);
});

test("runJob reports not started before start", async () => {
  const client = notStartedClient();
  await assert.rejects(() => client.runJob({ kind: "ping" }, { jobId: "test-ping" }), /not started/);
});
