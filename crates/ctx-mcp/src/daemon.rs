use crate::jobs::{JobError, JobManager};
use crate::rpc::{RpcMessage, jsonrpc_error, jsonrpc_result, write_response};
use crate::{tools, workspace};
use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use ctx_runtime::{
    RUNTIME_COMMAND_NAMES, RuntimeEvent, RuntimeJobCancelRequest, RuntimeJobGetRequest,
    RuntimeJobListRequest, RuntimeJobStartRequest,
};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};

#[derive(Debug, Args)]
pub(crate) struct DaemonArgs {
    /// Run the daemon over line-delimited JSON-RPC on stdin/stdout.
    #[arg(long)]
    stdio: bool,
    #[command(flatten)]
    serve: workspace::ServeArgs,
}

pub(crate) fn run(args: DaemonArgs) -> Result<()> {
    if !args.stdio {
        bail!("daemon currently supports only --stdio");
    }

    let runtime = Arc::new(tools::runtime(workspace::registry(&args.serve)?));
    let stdout = Arc::new(Mutex::new(io::stdout()));
    let event_stdout = Arc::clone(&stdout);
    let jobs = Arc::new(JobManager::new(runtime, move |event| {
        let _ = write_locked(&event_stdout, event_notification(event));
    }));
    let stdin = io::stdin();

    for line in stdin.lock().lines() {
        let line = line.context("failed to read stdin")?;
        if line.trim().is_empty() {
            continue;
        }
        let request: RpcMessage = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(err) => {
                write_locked(&stdout, jsonrpc_error(Value::Null, -32700, err.to_string()))?;
                continue;
            }
        };

        write_message_responses(&jobs, request, |value| write_locked(&stdout, value))?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn handle_message(runtime: Arc<tools::CtxRuntime>, message: RpcMessage) -> Vec<Value> {
    let jobs = Arc::new(JobManager::new(runtime, |_| {}));
    handle_message_with_jobs(&jobs, message)
}

#[cfg(test)]
fn handle_message_with_jobs(jobs: &Arc<JobManager>, message: RpcMessage) -> Vec<Value> {
    let mut responses = Vec::new();
    handle_message_with_sink(jobs, message, |value| {
        responses.push(value);
        Ok(())
    })
    .expect("in-memory response sink");
    responses
}

fn write_locked(out: &Arc<Mutex<impl Write>>, value: Value) -> Result<()> {
    let mut out = out.lock().map_err(|_| anyhow!("stdout lock poisoned"))?;
    write_response(&mut *out, value)
}

fn write_message_responses(
    jobs: &Arc<JobManager>,
    message: RpcMessage,
    mut emit: impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    handle_message_with_sink(jobs, message, &mut emit)
}

fn handle_message_with_sink(
    jobs: &Arc<JobManager>,
    message: RpcMessage,
    mut emit: impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    let response_id = message.id.clone();
    match message.method.as_str() {
        "initialize" | "runtime/info" => emit_response(response_id, runtime_info, &mut emit),
        "runtime/tools/list" => emit_response(
            response_id,
            || json!({ "tools": jobs.runtime().tool_specs() }),
            &mut emit,
        ),
        "runtime/jobs/start" => handle_job_start(jobs, response_id, message.params, &mut emit),
        "runtime/jobs/get" => handle_job_get(jobs, response_id, message.params, &mut emit),
        "runtime/jobs/list" => handle_job_list(jobs, response_id, message.params, &mut emit),
        "runtime/jobs/cancel" => handle_job_cancel(jobs, response_id, message.params, &mut emit),
        _ => emit_error(response_id, -32601, "method not found", &mut emit),
    }
}

fn handle_job_start(
    jobs: &Arc<JobManager>,
    response_id: Option<Value>,
    params: Value,
    emit: &mut impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    let request = match serde_json::from_value::<RuntimeJobStartRequest>(params) {
        Ok(request) => request,
        Err(err) => return emit_error(response_id, -32602, err.to_string(), emit),
    };
    match jobs.start(request) {
        Ok(job) => emit_response_value(response_id, json!({ "job": job }), emit),
        Err(err) => emit_job_error(response_id, err, emit),
    }
}

fn handle_job_get(
    jobs: &JobManager,
    response_id: Option<Value>,
    params: Value,
    emit: &mut impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    let request = match serde_json::from_value::<RuntimeJobGetRequest>(params) {
        Ok(request) => request,
        Err(err) => return emit_error(response_id, -32602, err.to_string(), emit),
    };
    match jobs.get(request) {
        Ok(job) => emit_response_value(response_id, json!({ "job": job }), emit),
        Err(err) => emit_job_error(response_id, err, emit),
    }
}

fn handle_job_list(
    jobs: &JobManager,
    response_id: Option<Value>,
    params: Value,
    emit: &mut impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    let request = match serde_json::from_value::<RuntimeJobListRequest>(params) {
        Ok(request) => request,
        Err(err) => return emit_error(response_id, -32602, err.to_string(), emit),
    };
    emit_response_value(response_id, json!({ "jobs": jobs.list(request) }), emit)
}

fn handle_job_cancel(
    jobs: &JobManager,
    response_id: Option<Value>,
    params: Value,
    emit: &mut impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    let request = match serde_json::from_value::<RuntimeJobCancelRequest>(params) {
        Ok(request) => request,
        Err(err) => return emit_error(response_id, -32602, err.to_string(), emit),
    };
    match jobs.cancel(&request.job_id) {
        Ok((cancellation_requested, job)) => emit_response_value(
            response_id,
            json!({ "cancellation_requested": cancellation_requested, "job": job }),
            emit,
        ),
        Err(err) => emit_job_error(response_id, err, emit),
    }
}

fn emit_job_error(
    response_id: Option<Value>,
    error: JobError,
    emit: &mut impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    emit_error(response_id, error.code(), error.to_string(), emit)
}

fn emit_response(
    id: Option<Value>,
    result: impl FnOnce() -> Value,
    emit: &mut impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    if let Some(id) = id {
        emit(jsonrpc_result(id, result()))?;
    }
    Ok(())
}

fn emit_response_value(
    id: Option<Value>,
    result: Value,
    emit: &mut impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    emit_response(id, || result, emit)
}

fn emit_error(
    id: Option<Value>,
    code: i64,
    message: impl Into<String>,
    emit: &mut impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    if let Some(id) = id {
        emit(jsonrpc_error(id, code, message))?;
    }
    Ok(())
}

fn event_notification(event: RuntimeEvent) -> Value {
    json!({ "jsonrpc": "2.0", "method": "runtime/event", "params": event })
}

fn runtime_info() -> Value {
    json!({
        "protocol": "ctx-runtime",
        "protocolVersion": "3",
        "serverInfo": { "name": "ctx-mcp-daemon", "version": env!("CARGO_PKG_VERSION") },
        "capabilities": {
            "transport": { "jsonrpc": "2.0", "framing": "ndjson" },
            "events": { "method": "runtime/event" },
            "jobs": {
                "methods": [
                    "runtime/jobs/start",
                    "runtime/jobs/get",
                    "runtime/jobs/list",
                    "runtime/jobs/cancel"
                ],
                "commandKinds": RUNTIME_COMMAND_NAMES
            }
        }
    })
}

#[cfg(test)]
mod tests;
