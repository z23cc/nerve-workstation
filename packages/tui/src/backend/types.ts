export type * from "./protocol.generated.ts";

export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonValue[] | { [key: string]: JsonValue };
export type JsonObject = { [key: string]: JsonValue };

import type {
  RuntimeCommand,
  RuntimeEvent,
  RuntimeJobCancelResponse,
  RuntimeJobSnapshot,
  RuntimeInfo,
  RuntimeToolSpec,
} from "./protocol.generated.ts";

export type RuntimeJob = RuntimeJobSnapshot;

export interface WorkstationBackend {
  start(): Promise<void>;
  stop(): Promise<void>;
  info(): Promise<RuntimeInfo>;
  listTools(): Promise<RuntimeToolSpec[]>;
  startJob(command: RuntimeCommand, options?: { jobId?: string }): Promise<RuntimeJob>;
  getJob(jobId: string, options?: { includeResult?: boolean }): Promise<RuntimeJob>;
  listJobs(options?: { includeTerminal?: boolean; includeResults?: boolean; limit?: number }): Promise<RuntimeJob[]>;
  cancelJob(jobId: string): Promise<RuntimeJobCancelResponse>;
  runJob(command: RuntimeCommand, options?: { jobId?: string }): Promise<JsonValue>;
  onEvent(listener: (event: RuntimeEvent) => void): () => void;
}
