//! Daemon-level integration tests for the DA-2 delegate runtime: trust-context
//! refusal, agent/cwd validation, and the streaming progress + outcome path.
//! Driven through the real router/job plumbing with a canned launcher so no
//! external agent CLI is spawned.

use super::{
    Arc, Mutex, dispatch, json, output_router, output_router_with_delegate, response_with_id, rpc,
    runtime_with_file, wait_for_job_event,
};

#[test]
fn delegate_start_refused_by_default_trust_context() {
    // The default daemon refuses delegation (refusing launcher). The job reaches
    // a terminal failure whose message points at the --allow-delegate lift.
    let fixture = runtime_with_file();
    let (router, output) = output_router(Arc::clone(&fixture.runtime));
    dispatch(
        &router,
        &output,
        rpc(
            json!(1),
            "runtime/jobs/start",
            json!({
                "job_id": "delegate-job",
                "command": {
                    "kind": "delegate.start",
                    "agent": "codex",
                    "task": "add a test"
                }
            }),
        ),
    );
    let failed = wait_for_job_event(&output, "job_failed", "delegate-job");
    let message = failed["params"]["error"]["message"]
        .as_str()
        .expect("error message string");
    assert!(message.contains("disabled"), "{message}");
    assert!(message.contains("--allow-delegate"), "{message}");
    assert!(message.contains("codex"), "{message}");

    let observed = dispatch(
        &router,
        &output,
        rpc(
            json!(2),
            "runtime/jobs/get",
            json!({ "job_id": "delegate-job" }),
        ),
    );
    let job = &response_with_id(&observed, json!(2))["result"]["job"];
    assert_eq!(job["status"], "failed");
    assert_eq!(job["command"], "delegate.start");
}

#[test]
fn delegate_start_unknown_agent_is_rejected() {
    // A bad agent name fails before any launcher is consulted, even with a real
    // (canned) launcher injected.
    let fixture = runtime_with_file();
    let (router, output) = output_router_with_delegate(
        Arc::clone(&fixture.runtime),
        Arc::new(CannedLauncher::ok("")),
    );
    dispatch(
        &router,
        &output,
        rpc(
            json!(1),
            "runtime/jobs/start",
            json!({
                "job_id": "bad-agent",
                "command": { "kind": "delegate.start", "agent": "rovo", "task": "x" }
            }),
        ),
    );
    let failed = wait_for_job_event(&output, "job_failed", "bad-agent");
    let message = failed["params"]["error"]["message"]
        .as_str()
        .expect("error message");
    assert!(message.contains("unknown delegate agent"), "{message}");
}

#[test]
fn delegate_start_rejects_cwd_escape() {
    let fixture = runtime_with_file();
    let (router, output) = output_router_with_delegate(
        Arc::clone(&fixture.runtime),
        Arc::new(CannedLauncher::ok("")),
    );
    dispatch(
        &router,
        &output,
        rpc(
            json!(1),
            "runtime/jobs/start",
            json!({
                "job_id": "escape-job",
                "command": {
                    "kind": "delegate.start",
                    "agent": "codex",
                    "task": "x",
                    "cwd": "../etc"
                }
            }),
        ),
    );
    let failed = wait_for_job_event(&output, "job_failed", "escape-job");
    let message = failed["params"]["error"]["message"]
        .as_str()
        .expect("error message");
    assert!(message.contains("cwd rejected"), "{message}");
    assert!(message.contains(".."), "{message}");
}

/// A launcher that records the argv it was handed and replays a canned stdout
/// through the trait's default line-replay streaming path — exercising the
/// delegate parser without spawning a real process.
struct CannedLauncher {
    stdout: String,
    exit_code: Option<i32>,
    seen: Mutex<Option<crate::sandbox::CommandSpec>>,
}

impl CannedLauncher {
    fn ok(stdout: &str) -> Self {
        Self {
            stdout: stdout.to_string(),
            exit_code: Some(0),
            seen: Mutex::new(None),
        }
    }
}

impl crate::sandbox::SandboxLauncher for CannedLauncher {
    fn launch(
        &self,
        spec: &crate::sandbox::CommandSpec,
        _policy: &crate::sandbox::SandboxPolicy,
        _cancel: &nerve_core::CancelToken,
    ) -> anyhow::Result<crate::sandbox::Output> {
        *self.seen.lock().expect("seen lock") = Some(crate::sandbox::CommandSpec {
            command: spec.command.clone(),
            args: spec.args.clone(),
        });
        Ok(crate::sandbox::Output {
            exit_code: self.exit_code,
            stdout: self.stdout.clone(),
            stderr: String::new(),
            timed_out: false,
        })
    }
}
