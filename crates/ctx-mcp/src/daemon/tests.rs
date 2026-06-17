use super::*;
use crate::workspace::{args_with, registry};
use std::fs;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

struct RuntimeFixture {
    _root: tempfile::TempDir,
    runtime: Arc<tools::CtxRuntime>,
}

fn runtime_with_file() -> RuntimeFixture {
    let root = tempfile::tempdir().expect("tempdir");
    fs::write(root.path().join("notes.txt"), "alpha beta\n").expect("write notes");
    let runtime = tools::runtime(
        registry(&args_with(vec![root.path().to_path_buf()], Vec::new())).expect("registry"),
    );
    RuntimeFixture {
        _root: root,
        runtime: Arc::new(runtime),
    }
}

fn rpc(id: impl Into<Option<Value>>, method: &str, params: Value) -> RpcMessage {
    RpcMessage {
        id: id.into(),
        method: method.to_string(),
        params,
    }
}

fn output_manager(runtime: Arc<tools::CtxRuntime>) -> (Arc<JobManager>, Arc<Mutex<Vec<Value>>>) {
    let output = Arc::new(Mutex::new(Vec::new()));
    let event_output = Arc::clone(&output);
    let jobs = Arc::new(JobManager::new(runtime, move |event| {
        event_output
            .lock()
            .expect("output lock")
            .push(event_notification(event));
    }));
    (jobs, output)
}

fn dispatch(
    jobs: &Arc<JobManager>,
    output: &Arc<Mutex<Vec<Value>>>,
    message: RpcMessage,
) -> Vec<Value> {
    handle_message_with_sink(jobs, message, |value| {
        output.lock().expect("output lock").push(value);
        Ok(())
    })
    .expect("dispatch");
    output.lock().expect("output lock").clone()
}

fn response_with_id(output: &[Value], id: Value) -> &Value {
    output
        .iter()
        .find(|value| value.get("id") == Some(&id))
        .expect("response id")
}

fn wait_for_job_event(output: &Arc<Mutex<Vec<Value>>>, event_type: &str, job_id: &str) -> Value {
    for _ in 0..100 {
        let found = output
            .lock()
            .expect("output lock")
            .iter()
            .find_map(|value| {
                let params = value.get("params")?;
                let matches_type = params.get("type") == Some(&json!(event_type));
                let matches_job = params.get("job_id") == Some(&json!(job_id));
                (matches_type && matches_job).then(|| value.clone())
            });
        if let Some(value) = found {
            return value;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {event_type} on {job_id}");
}

#[test]
fn initialize_returns_runtime_info() {
    let fixture = runtime_with_file();
    let responses = handle_message(
        Arc::clone(&fixture.runtime),
        rpc(json!(1), "initialize", json!({})),
    );
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["result"]["protocol"], "ctx-runtime");
    assert_eq!(responses[0]["result"]["protocolVersion"], "2");
    assert_eq!(
        responses[0]["result"]["capabilities"]["jobs"]["methods"][0],
        "runtime/jobs/start"
    );
}

#[test]
fn runtime_command_emits_events_and_response() {
    let fixture = runtime_with_file();
    let responses = handle_message(
        Arc::clone(&fixture.runtime),
        rpc(
            json!(7),
            "runtime/command",
            json!({
                "command_id": "search-1",
                "command": {
                    "kind": "tool.call",
                    "name": "file_search",
                    "arguments": { "pattern": "alpha", "mode": "content" }
                }
            }),
        ),
    );

    assert_eq!(responses.len(), 3);
    assert_eq!(responses[0]["method"], "runtime/event");
    assert_eq!(responses[0]["params"]["type"], "command_started");
    assert_eq!(responses[0]["params"]["command_id"], "search-1");
    assert_eq!(responses[1]["params"]["type"], "command_completed");
    assert_eq!(
        responses[2]["result"]["structuredContent"]["content_matches"][0]["path"],
        "notes.txt"
    );
}

#[test]
fn runtime_command_failure_emits_failed_event() {
    let fixture = runtime_with_file();
    let responses = handle_message(
        Arc::clone(&fixture.runtime),
        rpc(
            json!(9),
            "runtime/command",
            json!({ "command": { "kind": "tool.call", "name": "missing_tool" } }),
        ),
    );

    assert_eq!(responses.len(), 3);
    assert_eq!(responses[0]["params"]["command_id"], "9");
    assert_eq!(responses[1]["params"]["type"], "command_failed");
    assert!(responses[2]["error"]["message"].is_string());
}

#[test]
fn malformed_runtime_command_is_invalid_params() {
    let fixture = runtime_with_file();
    let responses = handle_message(
        Arc::clone(&fixture.runtime),
        rpc(json!(3), "runtime/command", json!({})),
    );
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["error"]["code"], -32602);
}

#[test]
fn notification_command_emits_events_without_response() {
    let fixture = runtime_with_file();
    let responses = handle_message(
        Arc::clone(&fixture.runtime),
        rpc(
            None,
            "runtime/command",
            json!({ "command": { "kind": "ping" } }),
        ),
    );
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["params"]["command_id"], "anonymous");
    assert_eq!(responses[0]["params"]["type"], "command_started");
    assert_eq!(responses[1]["params"]["type"], "command_completed");
}

#[test]
fn explicit_null_id_gets_response() {
    let fixture = runtime_with_file();
    let request: RpcMessage = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": null,
        "method": "runtime/command",
        "params": { "command": { "kind": "ping" } }
    }))
    .expect("rpc message");
    let responses = handle_message(Arc::clone(&fixture.runtime), request);
    assert_eq!(responses.len(), 3);
    assert!(responses[2].get("result").is_some());
    assert_eq!(responses[2]["id"], Value::Null);
}

#[test]
fn command_started_is_emitted_before_execution() {
    let fixture = runtime_with_file();
    let jobs = Arc::new(JobManager::new(Arc::clone(&fixture.runtime), |_| {}));
    let mut observed = Vec::new();
    let result = handle_message_with_sink(
        &jobs,
        rpc(
            json!(1),
            "runtime/command",
            json!({ "command": { "kind": "ping" } }),
        ),
        |value| {
            observed.push(value);
            anyhow::bail!("stop after first write")
        },
    );
    assert!(result.is_err());
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0]["params"]["type"], "command_started");
}

#[test]
fn job_start_get_and_list_track_completed_job() {
    let fixture = runtime_with_file();
    let (jobs, output) = output_manager(Arc::clone(&fixture.runtime));
    let observed = dispatch(
        &jobs,
        &output,
        rpc(
            json!(1),
            "runtime/jobs/start",
            json!({ "job_id": "job-ok", "command": { "kind": "ping" } }),
        ),
    );
    assert_eq!(
        response_with_id(&observed, json!(1))["result"]["job"]["status"],
        "running"
    );
    assert_eq!(observed[0]["params"]["type"], "job_started");
    wait_for_job_event(&output, "job_completed", "job-ok");

    let observed = dispatch(
        &jobs,
        &output,
        rpc(json!(2), "runtime/jobs/get", json!({ "job_id": "job-ok" })),
    );
    let job = &response_with_id(&observed, json!(2))["result"]["job"];
    assert_eq!(job["status"], "completed");
    assert_eq!(job["result"]["status"], "ok");

    let observed = dispatch(
        &jobs,
        &output,
        rpc(
            json!(3),
            "runtime/jobs/list",
            json!({ "include_terminal": true, "include_results": false }),
        ),
    );
    let listed = &response_with_id(&observed, json!(3))["result"]["jobs"];
    assert_eq!(listed.as_array().expect("jobs").len(), 1);
    assert_eq!(listed[0]["job_id"], "job-ok");
    assert!(listed[0]["result"].is_null());

    let observed = dispatch(
        &jobs,
        &output,
        rpc(
            json!(4),
            "runtime/jobs/list",
            json!({ "include_terminal": false }),
        ),
    );
    assert_eq!(
        response_with_id(&observed, json!(4))["result"]["jobs"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn job_duplicate_and_unknown_errors_use_protocol_codes() {
    let fixture = runtime_with_file();
    let (jobs, output) = output_manager(Arc::clone(&fixture.runtime));
    dispatch(
        &jobs,
        &output,
        rpc(
            json!(1),
            "runtime/jobs/start",
            json!({ "job_id": "same", "command": { "kind": "ping" } }),
        ),
    );
    let observed = dispatch(
        &jobs,
        &output,
        rpc(
            json!(2),
            "runtime/jobs/start",
            json!({ "job_id": "same", "command": { "kind": "ping" } }),
        ),
    );
    assert_eq!(
        response_with_id(&observed, json!(2))["error"]["code"],
        -32009
    );

    let observed = dispatch(
        &jobs,
        &output,
        rpc(json!(3), "runtime/jobs/get", json!({ "job_id": "missing" })),
    );
    assert_eq!(
        response_with_id(&observed, json!(3))["error"]["code"],
        -32004
    );

    let observed = dispatch(
        &jobs,
        &output,
        rpc(
            json!(4),
            "runtime/jobs/cancel",
            json!({ "job_id": "missing" }),
        ),
    );
    assert_eq!(
        response_with_id(&observed, json!(4))["error"]["code"],
        -32004
    );
}

#[test]
fn job_failure_stores_error_and_emits_terminal_event() {
    let fixture = runtime_with_file();
    let (jobs, output) = output_manager(Arc::clone(&fixture.runtime));
    dispatch(
        &jobs,
        &output,
        rpc(
            json!(1),
            "runtime/jobs/start",
            json!({
                "job_id": "job-fail",
                "command": { "kind": "tool.call", "name": "missing_tool" }
            }),
        ),
    );
    let failed = wait_for_job_event(&output, "job_failed", "job-fail");
    assert!(failed["params"]["error"]["message"].is_string());

    let observed = dispatch(
        &jobs,
        &output,
        rpc(
            json!(2),
            "runtime/jobs/get",
            json!({ "job_id": "job-fail" }),
        ),
    );
    let job = &response_with_id(&observed, json!(2))["result"]["job"];
    assert_eq!(job["status"], "failed");
    assert!(job["error"]["message"].is_string());
}

#[test]
fn job_cancel_requests_token_and_emits_cancelled() {
    let fixture = runtime_with_file();
    let output = Arc::new(Mutex::new(Vec::new()));
    let event_output = Arc::clone(&output);
    let (progress_tx, progress_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let jobs = Arc::new(JobManager::new(
        Arc::clone(&fixture.runtime),
        move |event| {
            let block = matches!(event, RuntimeEvent::JobProgress { ref job_id, .. } if job_id == "cancel-me");
            event_output
                .lock()
                .expect("output lock")
                .push(event_notification(event));
            if block {
                progress_tx.send(()).expect("progress send");
                release_rx
                    .lock()
                    .expect("release lock")
                    .recv()
                    .expect("release");
            }
        },
    ));

    dispatch(
        &jobs,
        &output,
        rpc(
            json!(1),
            "runtime/jobs/start",
            json!({ "job_id": "cancel-me", "command": { "kind": "ping" } }),
        ),
    );
    progress_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("job progress");

    let observed = dispatch(
        &jobs,
        &output,
        rpc(
            json!(2),
            "runtime/jobs/cancel",
            json!({ "job_id": "cancel-me" }),
        ),
    );
    let cancel = &response_with_id(&observed, json!(2))["result"];
    assert_eq!(cancel["cancellation_requested"], true);
    assert_eq!(cancel["job"]["status"], "cancelling");
    wait_for_job_event(&output, "job_cancel_requested", "cancel-me");
    release_tx.send(()).expect("release job");
    wait_for_job_event(&output, "job_cancelled", "cancel-me");

    let observed = dispatch(
        &jobs,
        &output,
        rpc(
            json!(3),
            "runtime/jobs/get",
            json!({ "job_id": "cancel-me" }),
        ),
    );
    assert_eq!(
        response_with_id(&observed, json!(3))["result"]["job"]["status"],
        "cancelled"
    );
}

#[test]
fn job_start_notification_emits_events_without_response() {
    let fixture = runtime_with_file();
    let (jobs, output) = output_manager(Arc::clone(&fixture.runtime));
    dispatch(
        &jobs,
        &output,
        rpc(
            None,
            "runtime/jobs/start",
            json!({ "job_id": "notify", "command": { "kind": "ping" } }),
        ),
    );
    wait_for_job_event(&output, "job_completed", "notify");
    let observed = output.lock().expect("output lock").clone();
    assert!(observed.iter().all(|value| value.get("id").is_none()));
    assert!(
        observed
            .iter()
            .any(|value| value["params"]["type"] == "job_started")
    );
    assert!(
        observed
            .iter()
            .any(|value| value["params"]["type"] == "job_progress")
    );
    assert!(
        observed
            .iter()
            .any(|value| value["params"]["type"] == "job_completed")
    );
}
