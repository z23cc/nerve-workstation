import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { existsSync } from "node:fs";
import { createInterface, type Interface } from "node:readline";
import { dirname, resolve } from "node:path";
import type {
  JsonObject,
  JsonValue,
  RuntimeCommand,
  RuntimeEvent,
  RuntimeInfo,
  RuntimeJob,
  RuntimeToolSpec,
  WorkstationBackend,
} from "./types.ts";

interface RpcSuccess {
  jsonrpc: "2.0";
  id: string | number | null;
  result: JsonValue;
}

interface RpcFailure {
  jsonrpc: "2.0";
  id: string | number | null;
  error: { code: number; message: string; data?: JsonValue };
}

interface RpcNotification {
  jsonrpc: "2.0";
  method: string;
  params?: JsonValue;
}

type RpcMessage = RpcSuccess | RpcFailure | RpcNotification;

type Pending = {
  resolve: (value: JsonValue) => void;
  reject: (error: Error) => void;
};

type JobWaiter = {
  resolve: (event: Extract<RuntimeEvent, { type: "job_completed" | "job_failed" | "job_cancelled" }>) => void;
  reject: (error: Error) => void;
};

export interface CtxDaemonClientOptions {
  root: string;
  binary?: string;
  cwd?: string;
  extraArgs?: string[];
  env?: NodeJS.ProcessEnv;
}

export class CtxDaemonClient implements WorkstationBackend {
  #options: Required<Omit<CtxDaemonClientOptions, "env">> & { env?: NodeJS.ProcessEnv };
  #child: ChildProcessWithoutNullStreams | undefined;
  #stdout: Interface | undefined;
  #nextId = 1;
  #nextJobId = 1;
  #pending = new Map<string, Pending>();
  #jobWaiters = new Map<string, JobWaiter[]>();
  #listeners = new Set<(event: RuntimeEvent) => void>();
  #stderr = "";

  constructor(options: CtxDaemonClientOptions) {
    const cwd = options.cwd ?? process.cwd();
    this.#options = {
      binary: options.binary ?? defaultBinary(),
      root: resolve(cwd, options.root),
      cwd,
      extraArgs: options.extraArgs ?? [],
      env: options.env,
    };
  }

  async start(): Promise<void> {
    if (this.#child) return;
    const child = spawn(
      this.#options.binary,
      ["daemon", "--stdio", "--root", this.#options.root, ...this.#options.extraArgs],
      {
        cwd: this.#options.cwd,
        env: { ...process.env, ...this.#options.env },
        stdio: ["pipe", "pipe", "pipe"],
      },
    );
    this.#child = child;
    this.#stdout = createInterface({ input: child.stdout });
    this.#stdout.on("line", (line) => this.#handleLine(line));
    child.stderr.on("data", (chunk) => {
      this.#stderr += chunk.toString();
    });
    child.on("error", (error) => this.#rejectAll(error));
    child.on("exit", (code, signal) => this.#rejectAll(new Error(`ctx-mcp daemon exited: code=${code} signal=${signal}`)));
    await this.info();
  }

  async stop(): Promise<void> {
    const child = this.#child;
    this.#child = undefined;
    this.#stdout?.close();
    this.#stdout = undefined;
    this.#rejectAll(new Error("ctx-mcp daemon stopped"));
    if (!child || child.killed) return;
    child.stdin.end();
    child.kill("SIGTERM");
  }

  async info(): Promise<RuntimeInfo> {
    return (await this.#request("runtime/info")) as RuntimeInfo;
  }

  async listTools(): Promise<RuntimeToolSpec[]> {
    const response = (await this.#request("runtime/tools/list")) as JsonObject;
    return (response.tools as RuntimeToolSpec[] | undefined) ?? [];
  }

  async startJob(command: RuntimeCommand, options: { jobId?: string } = {}): Promise<RuntimeJob> {
    const response = (await this.#request("runtime/jobs/start", {
      job_id: options.jobId,
      command: command as unknown as JsonObject,
    })) as JsonObject;
    return response.job as unknown as RuntimeJob;
  }

  async getJob(jobId: string, options: { includeResult?: boolean } = {}): Promise<RuntimeJob> {
    const response = (await this.#request("runtime/jobs/get", {
      job_id: jobId,
      include_result: options.includeResult ?? true,
    })) as JsonObject;
    return response.job as unknown as RuntimeJob;
  }

  async listJobs(
    options: { includeTerminal?: boolean; includeResults?: boolean; limit?: number } = {},
  ): Promise<RuntimeJob[]> {
    const response = (await this.#request("runtime/jobs/list", {
      include_terminal: options.includeTerminal ?? true,
      include_results: options.includeResults ?? false,
      limit: options.limit ?? 100,
    })) as JsonObject;
    return (response.jobs as unknown as RuntimeJob[] | undefined) ?? [];
  }

  async cancelJob(jobId: string): Promise<{ cancellation_requested: boolean; job: RuntimeJob }> {
    const response = (await this.#request("runtime/jobs/cancel", { job_id: jobId })) as JsonObject;
    return response as unknown as { cancellation_requested: boolean; job: RuntimeJob };
  }

  async runJob(command: RuntimeCommand, options: { jobId?: string } = {}): Promise<JsonValue> {
    const jobId = options.jobId ?? `tui-job-${this.#nextJobId++}`;
    const terminalEvent = this.#waitForTerminalJobEvent(jobId);
    try {
      await this.startJob(command, { jobId });
      await terminalEvent;
      const job = await this.getJob(jobId, { includeResult: true });
      if (job.status === "completed") return job.result ?? null;
      throw new Error(job.error?.message ?? `runtime job ${job.status}: ${jobId}`);
    } catch (error) {
      this.#removeJobWaiter(jobId, terminalEvent);
      throw error;
    }
  }

  onEvent(listener: (event: RuntimeEvent) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  async #request(method: string, params?: JsonObject): Promise<JsonValue> {
    const child = this.#child;
    if (!child) throw new Error("ctx-mcp daemon is not started");
    const id = this.#nextId++;
    const payload = JSON.stringify({ jsonrpc: "2.0", id, method, params });
    return new Promise<JsonValue>((resolve, reject) => {
      this.#pending.set(String(id), { resolve, reject });
      child.stdin.write(`${payload}\n`, (error) => {
        if (!error) return;
        this.#pending.delete(String(id));
        reject(error);
      });
    });
  }

  #handleLine(line: string): void {
    if (!line.trim()) return;
    let message: RpcMessage;
    try {
      message = JSON.parse(line) as RpcMessage;
    } catch (error) {
      this.#rejectAll(new Error(`invalid daemon JSON: ${String(error)}`));
      return;
    }
    if ("method" in message) {
      this.#handleNotification(message);
      return;
    }
    this.#handleResponse(message);
  }

  #handleNotification(message: RpcNotification): void {
    if (message.method !== "runtime/event") return;
    const event = message.params as RuntimeEvent;
    this.#handleJobEvent(event);
    for (const listener of this.#listeners) listener(event);
  }

  #handleResponse(message: RpcSuccess | RpcFailure): void {
    const key = String(message.id);
    const pending = this.#pending.get(key);
    if (!pending) return;
    this.#pending.delete(key);
    if ("error" in message) {
      pending.reject(new Error(message.error.message));
      return;
    }
    pending.resolve(message.result);
  }

  #waitForTerminalJobEvent(
    jobId: string,
  ): Promise<Extract<RuntimeEvent, { type: "job_completed" | "job_failed" | "job_cancelled" }>> {
    return new Promise((resolve, reject) => {
      const waiters = this.#jobWaiters.get(jobId) ?? [];
      waiters.push({ resolve, reject });
      this.#jobWaiters.set(jobId, waiters);
    });
  }

  #removeJobWaiter(jobId: string, promise: Promise<unknown>): void {
    void promise;
    const waiters = this.#jobWaiters.get(jobId);
    if (!waiters || waiters.length === 0) return;
    waiters.shift();
    if (waiters.length === 0) this.#jobWaiters.delete(jobId);
  }

  #handleJobEvent(event: RuntimeEvent): void {
    if (event.type !== "job_completed" && event.type !== "job_failed" && event.type !== "job_cancelled") return;
    const waiters = this.#jobWaiters.get(event.job_id);
    if (!waiters) return;
    this.#jobWaiters.delete(event.job_id);
    for (const waiter of waiters) waiter.resolve(event);
  }

  #rejectAll(error: Error): void {
    const suffix = this.#stderr.trim();
    const reason = suffix ? new Error(`${error.message}\n${suffix}`) : error;
    for (const pending of this.#pending.values()) pending.reject(reason);
    this.#pending.clear();
    for (const waiters of this.#jobWaiters.values()) {
      for (const waiter of waiters) waiter.reject(reason);
    }
    this.#jobWaiters.clear();
  }
}

function defaultBinary(): string {
  const name = process.platform === "win32" ? "ctx-mcp.exe" : "ctx-mcp";
  for (let dir = process.cwd(); ; dir = dirname(dir)) {
    const local = resolve(dir, "target", "debug", name);
    if (existsSync(local)) return local;
    const parent = dirname(dir);
    if (parent === dir) break;
  }
  return "ctx-mcp";
}
