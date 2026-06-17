export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonValue[] | { [key: string]: JsonValue };
export type JsonObject = { [key: string]: JsonValue };

export type RuntimeCommand =
  | { kind: "ping" }
  | { kind: "tool.list" }
  | { kind: "tool.call"; name: string; arguments?: JsonObject };

export type RuntimeJobStatus = "running" | "cancelling" | "completed" | "failed" | "cancelled";

export interface RuntimeJobError {
  kind: string;
  message: string;
}

export interface RuntimeJob {
  job_id: string;
  status: RuntimeJobStatus;
  command: string;
  tool_name?: string | null;
  created_at_ms: number;
  started_at_ms?: number | null;
  updated_at_ms: number;
  finished_at_ms?: number | null;
  cancel_requested: boolean;
  result?: JsonValue;
  error?: RuntimeJobError | null;
}

export type RuntimeEvent =
  | { command_id: string; type: "command_started"; command: string }
  | { command_id: string; type: "command_completed" }
  | { command_id: string; type: "command_failed"; error: string }
  | { job_id: string; type: "job_started"; command: string; tool_name?: string | null }
  | {
      job_id: string;
      type: "job_progress";
      stage: string;
      message: string;
      current?: number | null;
      total?: number | null;
    }
  | { job_id: string; type: "job_cancel_requested" }
  | { job_id: string; type: "job_completed" }
  | { job_id: string; type: "job_failed"; error: RuntimeJobError }
  | { job_id: string; type: "job_cancelled" };

export interface RuntimeInfo {
  protocol: "ctx-runtime";
  protocolVersion: string;
  serverInfo: { name: string; version: string };
  capabilities: JsonObject;
}

export interface RuntimeToolSpec {
  name: string;
  description?: string;
  inputSchema?: JsonObject;
  [key: string]: JsonValue | undefined;
}

export interface WorkstationBackend {
  start(): Promise<void>;
  stop(): Promise<void>;
  info(): Promise<RuntimeInfo>;
  listTools(): Promise<RuntimeToolSpec[]>;
  startJob(command: RuntimeCommand, options?: { jobId?: string }): Promise<RuntimeJob>;
  getJob(jobId: string, options?: { includeResult?: boolean }): Promise<RuntimeJob>;
  listJobs(options?: { includeTerminal?: boolean; includeResults?: boolean; limit?: number }): Promise<RuntimeJob[]>;
  cancelJob(jobId: string): Promise<{ cancellation_requested: boolean; job: RuntimeJob }>;
  runCommand(command: RuntimeCommand, options?: { commandId?: string }): Promise<JsonValue>;
  onEvent(listener: (event: RuntimeEvent) => void): () => void;
}
