//! [`NerveControl`] over the nerve daemon's runtime protocol.
//!
//! [`DelegateNerve`] spawns `nerve daemon --stdio --root <root> --allow-delegate`
//! and speaks JSON-RPC 2.0 over NDJSON (the same transport `nerve-tui` uses). A
//! WeChat message becomes a `delegate.start` (new chat) or `delegate.steer`
//! (follow-up) job; the reply is the `delegate_progress` text streamed for that
//! job, terminated by the `session_idle` event keyed by the job id.
//!
//! The wire-shaping (request envelope, command construction, event reduction) is
//! pure and unit-tested; only the process spawn + blocking read loop are not.

use crate::bridge::{BridgeError, NerveControl, NerveReply};
use nerve_proto::protocol::{RUNTIME_EVENT_METHOD, RUNTIME_JOB_START_METHOD};
use nerve_proto::{DelegateAutonomy, DelegateRole, RuntimeCommand, RuntimeEvent};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// Whether a turn is still streaming or has reached idle.
#[derive(Debug, PartialEq, Eq)]
enum TurnState {
    Continue,
    Done,
}

/// Build the `delegate.start` command for a new chat turn.
fn start_command(agent: &str, task: &str, autonomy: DelegateAutonomy) -> RuntimeCommand {
    RuntimeCommand::DelegateStart {
        agent: agent.to_string(),
        task: task.to_string(),
        // The WeChat bridge serves a single workspace (the daemon's --root); the sole
        // workspace resolves without an explicit name.
        workspace: None,
        cwd: None,
        autonomy,
        role: DelegateRole::Standard,
        model: None,
        mcp_enable: None,
    }
}

/// Build the `delegate.steer` command for a follow-up on an existing session.
fn steer_command(session_id: &str, message: &str) -> RuntimeCommand {
    RuntimeCommand::DelegateSteer {
        session_id: session_id.to_string(),
        message: message.to_string(),
    }
}

/// Render a JSON-RPC 2.0 request line (no trailing newline).
fn rpc_line(id: u64, method: &str, params: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string()
}

/// Parse a daemon stdout line into a [`RuntimeEvent`], or `None` for anything that
/// is not a `runtime/event` notification (RPC responses, other notifications).
fn parse_event_line(line: &str) -> Option<RuntimeEvent> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    if value.get("method").and_then(Value::as_str) != Some(RUNTIME_EVENT_METHOD) {
        return None;
    }
    // The notification params flatten the event alongside an `event_seq`; the
    // tagged-enum deserialize ignores the extra `event_seq` field.
    serde_json::from_value::<RuntimeEvent>(value.get("params")?.clone()).ok()
}

/// Fold one event into the accumulating reply for `session_key`: append
/// `delegate_progress` text for our job, stop on its `session_idle`.
fn reduce_event(event: &RuntimeEvent, session_key: &str, acc: &mut String) -> TurnState {
    match event {
        RuntimeEvent::DelegateProgress { job_id, text, .. } if job_id == session_key => {
            acc.push_str(text);
            TurnState::Continue
        }
        RuntimeEvent::SessionIdle { session_id } if session_id == session_key => TurnState::Done,
        _ => TurnState::Continue,
    }
}

/// A monotonically-unique job id for a new delegate session.
fn gen_job_id(counter: u64) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("wx-{}-{nanos}-{counter}", std::process::id())
}

/// The live daemon connection (child process + framed stdio).
struct DaemonConn {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    job_counter: u64,
}

impl DaemonConn {
    fn send(&mut self, params: Value) -> Result<(), BridgeError> {
        let line = rpc_line(self.next_id, RUNTIME_JOB_START_METHOD, params);
        self.next_id += 1;
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .map_err(|err| BridgeError::Nerve(format!("write to daemon: {err}")))
    }

    /// Read events until the turn for `session_key` goes idle, returning the
    /// accumulated `delegate_progress` text.
    fn await_turn(&mut self, session_key: &str) -> Result<String, BridgeError> {
        let mut acc = String::new();
        let mut line = String::new();
        loop {
            line.clear();
            let read = self
                .stdout
                .read_line(&mut line)
                .map_err(|err| BridgeError::Nerve(format!("read from daemon: {err}")))?;
            if read == 0 {
                return Err(BridgeError::Nerve("daemon closed the stream".into()));
            }
            if let Some(event) = parse_event_line(&line)
                && reduce_event(&event, session_key, &mut acc) == TurnState::Done
            {
                return Ok(acc);
            }
        }
    }
}

/// A [`NerveControl`] that drives delegated external CLI agents through a spawned
/// `nerve daemon`. Sequential by design (one turn at a time), matching the bridge's
/// single-threaded poll loop; interior mutability keeps the [`NerveControl`] `&self`
/// contract.
pub struct DelegateNerve {
    agent: String,
    autonomy: DelegateAutonomy,
    conn: RefCell<DaemonConn>,
}

impl DelegateNerve {
    /// Spawn `nerve daemon --stdio --root <root> --allow-delegate` and connect.
    /// `agent` is the delegate catalog name (`claude` / `codex`);
    /// `autonomy` is the posture granted to every delegated turn.
    pub fn spawn(
        nerve_bin: &str,
        root: &Path,
        agent: impl Into<String>,
        autonomy: DelegateAutonomy,
    ) -> Result<Self, BridgeError> {
        let mut child = Command::new(nerve_bin)
            .arg("daemon")
            .arg("--stdio")
            .arg("--root")
            .arg(root)
            .arg("--allow-delegate")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|err| BridgeError::Nerve(format!("spawn `{nerve_bin} daemon`: {err}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| BridgeError::Nerve("daemon stdin unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| BridgeError::Nerve("daemon stdout unavailable".into()))?;
        Ok(Self {
            agent: agent.into(),
            autonomy,
            conn: RefCell::new(DaemonConn {
                child,
                stdin,
                stdout: BufReader::new(stdout),
                next_id: 1,
                job_counter: 0,
            }),
        })
    }
}

impl Drop for DelegateNerve {
    fn drop(&mut self) {
        // Best-effort reap so the daemon child does not linger.
        let _ = self.conn.borrow_mut().child.kill();
    }
}

impl NerveControl for DelegateNerve {
    fn handle(
        &self,
        _chat_key: &str,
        _from_user_id: &str,
        existing: Option<&str>,
        text: &str,
    ) -> Result<NerveReply, BridgeError> {
        let mut conn = self.conn.borrow_mut();
        let (session_key, params) = match existing {
            None => {
                conn.job_counter += 1;
                let job_id = gen_job_id(conn.job_counter);
                let command = start_command(&self.agent, text, self.autonomy);
                let params = json!({ "job_id": job_id, "command": command });
                (job_id, params)
            }
            Some(session_id) => {
                let command = steer_command(session_id, text);
                (session_id.to_string(), json!({ "command": command }))
            }
        };
        conn.send(params)?;
        let reply = conn.await_turn(&session_key)?;
        Ok(NerveReply {
            session_id: session_key,
            text: reply,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_line_is_valid_jsonrpc() {
        let line = rpc_line(
            7,
            RUNTIME_JOB_START_METHOD,
            json!({ "command": { "kind": "x" } }),
        );
        let value: Value = serde_json::from_str(&line).expect("json");
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 7);
        assert_eq!(value["method"], RUNTIME_JOB_START_METHOD);
        assert_eq!(value["params"]["command"]["kind"], "x");
    }

    #[test]
    fn start_and_steer_commands_serialize_to_their_kinds() {
        let start = serde_json::to_value(start_command(
            "claude",
            "fix it",
            DelegateAutonomy::ReadOnly,
        ))
        .expect("serialize start");
        assert_eq!(start["kind"], "delegate.start");
        assert_eq!(start["agent"], "claude");
        assert_eq!(start["task"], "fix it");
        assert_eq!(start["autonomy"], "read_only");

        let steer = serde_json::to_value(steer_command("job-1", "and now tests")).expect("steer");
        assert_eq!(steer["kind"], "delegate.steer");
        assert_eq!(steer["session_id"], "job-1");
        assert_eq!(steer["message"], "and now tests");
    }

    #[test]
    fn parses_a_delegate_progress_notification_ignoring_event_seq() {
        let line = json!({
            "jsonrpc": "2.0",
            "method": RUNTIME_EVENT_METHOD,
            "params": { "event_seq": 4, "type": "delegate_progress", "job_id": "j1", "agent": "claude", "text": "hi" }
        })
        .to_string();
        let event = parse_event_line(&line).expect("event");
        assert!(matches!(event, RuntimeEvent::DelegateProgress { job_id, .. } if job_id == "j1"));
    }

    #[test]
    fn non_event_lines_are_ignored() {
        let response = json!({ "jsonrpc": "2.0", "id": 1, "result": {} }).to_string();
        assert!(parse_event_line(&response).is_none());
        assert!(parse_event_line("not json").is_none());
    }

    #[test]
    fn reduce_accumulates_our_progress_and_stops_on_our_idle() {
        let mut acc = String::new();
        // Progress for another job is ignored.
        let other = RuntimeEvent::DelegateProgress {
            job_id: "other".into(),
            agent: "claude".into(),
            text: "noise".into(),
        };
        assert_eq!(reduce_event(&other, "j1", &mut acc), TurnState::Continue);
        assert!(acc.is_empty());
        // Our progress accumulates.
        let ours = RuntimeEvent::DelegateProgress {
            job_id: "j1".into(),
            agent: "claude".into(),
            text: "answer".into(),
        };
        assert_eq!(reduce_event(&ours, "j1", &mut acc), TurnState::Continue);
        assert_eq!(acc, "answer");
        // Idle for another session keeps going; idle for ours ends the turn.
        let idle_other = RuntimeEvent::SessionIdle {
            session_id: "other".into(),
        };
        assert_eq!(
            reduce_event(&idle_other, "j1", &mut acc),
            TurnState::Continue
        );
        let idle_ours = RuntimeEvent::SessionIdle {
            session_id: "j1".into(),
        };
        assert_eq!(reduce_event(&idle_ours, "j1", &mut acc), TurnState::Done);
    }

    #[test]
    fn job_ids_are_unique_per_counter() {
        assert_ne!(gen_job_id(1), gen_job_id(2));
    }
}
