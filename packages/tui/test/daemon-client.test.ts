import { test } from "bun:test";
import assert from "node:assert/strict";
import { NerveClient } from "../src/backend/nerveClient.ts";

function notStartedClient(): NerveClient {
  return new NerveClient({ root: process.cwd(), binary: "nerve" });
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
