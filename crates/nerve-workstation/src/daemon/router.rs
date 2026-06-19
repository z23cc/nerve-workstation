use crate::jobs::{JobError, JobManager};
use crate::policy::Policy;
use crate::providers::ProviderRegistry;
use crate::rpc::{RpcMessage, jsonrpc_error, jsonrpc_result};
use crate::session::SessionStore;
use crate::tools;
use anyhow::Result;
use nerve_runtime::{
    RuntimeEvent, RuntimeJobCancelRequest, RuntimeJobGetRequest, RuntimeJobListRequest,
    RuntimeJobStartRequest,
    protocol::{
        RUNTIME_DAEMON_NAME, RUNTIME_EVENT_METHOD, RUNTIME_INFO_METHOD, RUNTIME_JOB_CANCEL_METHOD,
        RUNTIME_JOB_GET_METHOD, RUNTIME_JOB_LIST_METHOD, RUNTIME_JOB_START_METHOD,
        RUNTIME_TOOLS_LIST_METHOD, RuntimeEventNotification, RuntimeInfo,
    },
};
use serde_json::{Value, json};
use std::sync::Arc;

pub(super) struct RuntimeDaemonRouter {
    jobs: Arc<JobManager>,
}

impl RuntimeDaemonRouter {
    pub(super) fn new(
        runtime: Arc<tools::NerveRuntime>,
        registry: ProviderRegistry,
        policy: Policy,
        session_store: Option<SessionStore>,
        emit_notification: impl Fn(Value) + Send + Sync + 'static,
    ) -> Self {
        let jobs = Arc::new(JobManager::new(
            runtime,
            registry,
            policy,
            session_store,
            move |event| {
                emit_notification(runtime_event_notification(event));
            },
        ));
        Self { jobs }
    }

    #[cfg(test)]
    pub(super) fn with_jobs(jobs: Arc<JobManager>) -> Self {
        Self { jobs }
    }

    pub(super) fn handle_message(
        &self,
        message: RpcMessage,
        mut emit: impl FnMut(Value) -> Result<()>,
    ) -> Result<()> {
        let response_id = message.id.clone();
        match message.method.as_str() {
            "initialize" | RUNTIME_INFO_METHOD => {
                emit_response(response_id, runtime_info, &mut emit)
            }
            RUNTIME_TOOLS_LIST_METHOD => emit_response(
                response_id,
                || json!({ "tools": self.jobs.runtime().tool_specs() }),
                &mut emit,
            ),
            RUNTIME_JOB_START_METHOD => {
                self.handle_job_start(response_id, message.params, &mut emit)
            }
            RUNTIME_JOB_GET_METHOD => self.handle_job_get(response_id, message.params, &mut emit),
            RUNTIME_JOB_LIST_METHOD => self.handle_job_list(response_id, message.params, &mut emit),
            RUNTIME_JOB_CANCEL_METHOD => {
                self.handle_job_cancel(response_id, message.params, &mut emit)
            }
            _ => emit_error(response_id, -32601, "method not found", &mut emit),
        }
    }

    fn handle_job_start(
        &self,
        response_id: Option<Value>,
        params: Value,
        emit: &mut impl FnMut(Value) -> Result<()>,
    ) -> Result<()> {
        let request = match serde_json::from_value::<RuntimeJobStartRequest>(params) {
            Ok(request) => request,
            Err(err) => return emit_error(response_id, -32602, err.to_string(), emit),
        };
        match self.jobs.start(request) {
            Ok(job) => emit_response_value(response_id, json!({ "job": job }), emit),
            Err(err) => emit_job_error(response_id, err, emit),
        }
    }

    fn handle_job_get(
        &self,
        response_id: Option<Value>,
        params: Value,
        emit: &mut impl FnMut(Value) -> Result<()>,
    ) -> Result<()> {
        let request = match serde_json::from_value::<RuntimeJobGetRequest>(params) {
            Ok(request) => request,
            Err(err) => return emit_error(response_id, -32602, err.to_string(), emit),
        };
        match self.jobs.get(request) {
            Ok(job) => emit_response_value(response_id, json!({ "job": job }), emit),
            Err(err) => emit_job_error(response_id, err, emit),
        }
    }

    fn handle_job_list(
        &self,
        response_id: Option<Value>,
        params: Value,
        emit: &mut impl FnMut(Value) -> Result<()>,
    ) -> Result<()> {
        let request = match serde_json::from_value::<RuntimeJobListRequest>(params) {
            Ok(request) => request,
            Err(err) => return emit_error(response_id, -32602, err.to_string(), emit),
        };
        emit_response_value(
            response_id,
            json!({ "jobs": self.jobs.list(request) }),
            emit,
        )
    }

    fn handle_job_cancel(
        &self,
        response_id: Option<Value>,
        params: Value,
        emit: &mut impl FnMut(Value) -> Result<()>,
    ) -> Result<()> {
        let request = match serde_json::from_value::<RuntimeJobCancelRequest>(params) {
            Ok(request) => request,
            Err(err) => return emit_error(response_id, -32602, err.to_string(), emit),
        };
        match self.jobs.cancel(&request.job_id) {
            Ok((cancellation_requested, job)) => emit_response_value(
                response_id,
                json!({ "cancellation_requested": cancellation_requested, "job": job }),
                emit,
            ),
            Err(err) => emit_job_error(response_id, err, emit),
        }
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

pub(super) fn runtime_event_notification(event: RuntimeEvent) -> Value {
    // `event_seq` defaults to 0 here; assigning a real monotonic per-stream
    // sequence at emit time is a later wave. The carrier flattens the event, so
    // `params` stays backward compatible with clients reading the bare event.
    let params = RuntimeEventNotification::new(0, event);
    json!({ "jsonrpc": "2.0", "method": RUNTIME_EVENT_METHOD, "params": params })
}

fn runtime_info() -> Value {
    json!(RuntimeInfo::current(
        RUNTIME_DAEMON_NAME,
        env!("CARGO_PKG_VERSION")
    ))
}
