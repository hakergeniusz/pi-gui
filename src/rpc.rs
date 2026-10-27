//! Non-blocking RPC bridge to the pi coding agent.
//!
//! Two dedicated background threads own the `pi --mode rpc` child process:
//! a writer that blocks on the command channel, and a reader that blocks on
//! the child's stdout. The UI thread sends commands through a channel and
//! receives responses through per-request oneshot channels; async events are
//! forwarded to a channel the UI thread polls via `glib::timeout_add`. This
//! keeps the GTK main loop responsive — and, critically, writing a command
//! never waits for pi to produce output first: a single-threaded
//! write-then-blocking-read loop starves queued commands whenever pi is idle
//! and silent (e.g. a prompt typed after startup settles), which deadlocks
//! the agent forever.
//!
//! Wire format (see pi's docs/rpc-commands.md and docs/json.md): commands and
//! events are flat JSON objects with a `"type"` discriminator, e.g.
//! `{"type":"prompt","message":"hi"}` — NOT type+payload envelopes.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

/// A command sent to the pi agent. Field names serialize to camelCase per the
/// wire protocol (`modelId`, `sessionPath`, …).
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PiCommand {
    Prompt { message: String },
    Steer { message: String },
    Abort,
    GetState,
    GetMessages,
    GetAvailableModels,
    #[serde(rename_all = "camelCase")]
    SetModel { provider: String, model_id: String },
    #[serde(rename_all = "camelCase")]
    SetThinkingLevel { level: String },
    GetAvailableThinkingLevels,
    #[serde(rename_all = "camelCase")]
    SwitchSession { session_path: String },
    #[serde(rename_all = "camelCase")]
    SetAutoCompaction { enabled: bool },
    #[serde(rename_all = "camelCase")]
    SetSessionName { name: String },
}

/// An event streamed from the pi agent. Events are flat: the discriminator is
/// the top-level `"type"` and the fields sit next to it, not under a payload
/// key. Unknown events fall back to [`PiEvent::Other`]. The enum mirrors the
/// full protocol; not every field is consumed by the UI.
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
pub enum PiEvent {
    AgentStart,
    AgentEnd,
    AgentSettled { aborted: Option<bool> },
    TurnStart,
    TurnEnd,
    MessageStart { message: Option<serde_json::Value> },
    MessageUpdate {
        #[serde(rename = "assistantMessageEvent")]
        event: Option<serde_json::Value>,
    },
    MessageEnd { message: Option<serde_json::Value> },
    #[serde(rename_all = "camelCase")]
    ToolExecutionStart {
        tool_call_id: Option<String>,
        tool_name: Option<String>,
        args: Option<serde_json::Value>,
    },
    #[serde(rename_all = "camelCase")]
    ToolExecutionUpdate {
        tool_call_id: Option<String>,
        partial_result: Option<serde_json::Value>,
    },
    #[serde(rename_all = "camelCase")]
    ToolExecutionEnd {
        tool_call_id: Option<String>,
        result: Option<serde_json::Value>,
        is_error: Option<bool>,
    },
    EntryAppended { entry: Option<serde_json::Value> },
    #[serde(rename_all = "camelCase")]
    ExtensionUiRequest {
        id: String,
        method: String,
        #[serde(flatten)]
        extra: serde_json::Value,
    },
    #[serde(rename_all = "camelCase")]
    ExtensionError { extension_path: String, error: String },
    #[serde(rename_all = "camelCase")]
    AutoRetryStart { attempt: Option<u32>, error_message: Option<String> },
    #[serde(rename_all = "camelCase")]
    AutoRetryEnd { success: Option<bool>, attempt: Option<u32>, final_error: Option<String> },
    SessionInfoChanged { name: Option<String> },
    ThinkingLevelChanged { level: Option<String> },
    /// Command response records that were not correlated to a pending request.
    Response {
        id: Option<String>,
        command: Option<String>,
        success: Option<bool>,
        error: Option<String>,
    },
    /// Constructed by the bridge when the pi process exits — never arrives on
    /// the wire.
    #[serde(skip)]
    Disconnected,
    #[serde(other)]
    Other,
}

/// Internal message from UI thread to the RPC thread.
enum RpcMsg {
    Command { id: String, command: PiCommand, tx: mpsc::Sender<serde_json::Value> },
    ExtensionResponse { id: String, value: serde_json::Value },
}

/// Handle to the RPC bridge (cheaply cloneable, safe to use from any thread).
pub struct PiRpcClient {
    tx: mpsc::Sender<RpcMsg>,
    next_id: std::sync::atomic::AtomicU64,
}

impl Clone for PiRpcClient {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            next_id: std::sync::atomic::AtomicU64::new(
                self.next_id.load(std::sync::atomic::Ordering::Relaxed),
            ),
        }
    }
}

impl PiRpcClient {
    /// Send a command; returns a receiver that yields the response value.
    pub fn send(&self, command: PiCommand) -> mpsc::Receiver<serde_json::Value> {
        let id = format!(
            "req_{}",
            self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let (tx, rx) = mpsc::channel();
        let _ = self.tx.send(RpcMsg::Command { id, command, tx });
        rx
    }

    /// Deliver an extension UI response back to the RPC thread.
    pub fn extension_response(&self, id: &str, value: serde_json::Value) {
        let _ = self.tx.send(RpcMsg::ExtensionResponse {
            id: id.to_string(),
            value,
        });
    }
}

/// Receiver for async pi events (polled on the UI thread).
pub struct PiEventStream {
    rx: mpsc::Receiver<PiEvent>,
}

impl PiEventStream {
    pub fn try_event(&self) -> Option<PiEvent> {
        self.rx.try_recv().ok()
    }
}

/// Resolve the pi binary to launch.
///
/// Priority: `PI_GUI_PI_BIN` override, the canonical `~/.pi/agent/bin/pi`
/// install location, then bare `pi` (PATH lookup, which works when the shell
/// wrapper below has already put `~/.pi/agent/bin` on PATH).
fn resolve_pi_path() -> String {
    if let Ok(path) = std::env::var("PI_GUI_PI_BIN") {
        if !path.is_empty() {
            return path;
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = std::path::Path::new(&home).join(".pi/agent/bin/pi");
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    "pi".to_string()
}

/// Spawn `pi --mode rpc`.
///
/// Preferred path: through `zsh -c` that first sources the user's `~/.zshrc`.
/// This gives pi the full interactive-shell environment — the PATH entries
/// .zshrc sets up (including pi's own install dir) and API keys exported from
/// `~/.zsh_secrets` (which .zshrc sources). This makes the GUI work even when
/// launched from the GNOME desktop, where none of that is inherited. Output is
/// redirected while sourcing so .zshrc banners (e.g. hyfetch) cannot corrupt
/// the JSONL stream on stdout.
///
/// Set `PI_GUI_DIRECT=1` to skip the shell wrapper (debugging escape hatch).
/// If zsh or ~/.zshrc is unavailable, falls back to a direct spawn.
fn spawn_pi(cli_path: &str) -> std::io::Result<std::process::Child> {
    let direct = std::env::var("PI_GUI_DIRECT").map(|v| v == "1").unwrap_or(false);
    let zshrc = std::env::var("HOME")
        .ok()
        .map(|home| std::path::PathBuf::from(home).join(".zshrc"))
        .filter(|p| p.is_file());

    if !direct && zshrc.is_some() {
        let script = r#"source "$HOME/.zshrc" >/dev/null 2>&1; exec "$0" --mode rpc"#;
        let mut cmd = Command::new("zsh");
        cmd.arg("-c").arg(script).arg(cli_path);
        apply_stdio(&mut cmd);
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(err) => {
                eprintln!("[pi rpc] zsh wrapper spawn failed ({err}), falling back to direct spawn");
            }
        }
    }

    let mut cmd = Command::new(cli_path);
    cmd.arg("--mode").arg("rpc");
    apply_stdio(&mut cmd);
    cmd.spawn()
}

fn apply_stdio(cmd: &mut Command) {
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
}

/// Spawn the RPC bridge: child process plus dedicated writer and reader
/// threads.
///
/// The writer blocks on `cmd_rx`, so a queued command is written to pi's stdin
/// immediately regardless of whether pi has anything to say. The reader blocks
/// on the child's stdout and routes response records (by id) and async events
/// to their channels. Either thread exiting (broken pipe / pi exit) fails all
/// pending requests and emits [`PiEvent::Disconnected`].
pub fn spawn() -> Result<(PiRpcClient, PiEventStream), Box<dyn std::error::Error + Send + Sync>> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<RpcMsg>();
    let (event_tx, event_rx) = mpsc::channel::<PiEvent>();

    let client = PiRpcClient {
        tx: cmd_tx,
        next_id: std::sync::atomic::AtomicU64::new(0),
    };
    let stream = PiEventStream { rx: event_rx };

    let mut child = spawn_pi(&resolve_pi_path())?;

    let mut stdin = child.stdin.take().expect("failed to open stdin");
    let stdout = child.stdout.take().expect("failed to open stdout");
    let stderr = child.stderr.take().expect("failed to open stderr");

    // Surface stderr for diagnostics.
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().flatten() {
            eprintln!("[pi rpc] {line}");
        }
    });

    // Pending request id -> response sender, shared by both threads: the
    // writer registers senders, the reader resolves them.
    let pending: Arc<Mutex<HashMap<String, mpsc::Sender<serde_json::Value>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    thread::spawn({
        let pending = pending.clone();
        move || run_writer(&mut stdin, cmd_rx, &pending)
    });

    thread::spawn(move || run_reader(stdout, event_tx, &pending, child));

    Ok((client, stream))
}

/// Write side of the bridge: serialize commands and push them into pi's stdin
/// as soon as they arrive. Runs until the command channel closes or the pipe
/// breaks.
fn run_writer(
    stdin: &mut std::process::ChildStdin,
    cmd_rx: mpsc::Receiver<RpcMsg>,
    pending: &Arc<Mutex<HashMap<String, mpsc::Sender<serde_json::Value>>>>,
) {
    while let Ok(msg) = cmd_rx.recv() {
        match msg {
            RpcMsg::Command { id, command, tx } => {
                let Ok(mut value) = serde_json::to_value(&command) else {
                    let _ = tx.send(failure("failed to serialize command"));
                    continue;
                };
                if let Some(map) = value.as_object_mut() {
                    map.insert("id".into(), serde_json::json!(id));
                }
                let Ok(line) = serde_json::to_string(&value) else {
                    let _ = tx.send(failure("failed to serialize command"));
                    continue;
                };
                // Register the response sender BEFORE writing: run_reader drops
                // replies whose id is not in the map, so a fast reply could
                // otherwise arrive between the write and the insert and be lost.
                pending.lock().unwrap().insert(id.clone(), tx);
                let written = stdin
                    .write_all(format!("{line}\n").as_bytes())
                    .and_then(|_| stdin.flush());
                if written.is_err() {
                    if let Some(tx) = pending.lock().unwrap().remove(&id) {
                        let _ = tx.send(failure("failed to write to pi: pipe closed"));
                    }
                    break;
                }
            }
            RpcMsg::ExtensionResponse { id, mut value } => {
                let mut line_value =
                    serde_json::json!({ "type": "extension_ui_response", "id": id });
                if let (Some(map), Some(val_map)) =
                    (line_value.as_object_mut(), value.as_object_mut())
                {
                    for (k, v) in val_map.iter() {
                        map.insert(k.clone(), v.clone());
                    }
                }
                let line = serde_json::to_string(&line_value).unwrap();
                if stdin.write_all(format!("{line}\n").as_bytes()).and_then(|_| stdin.flush()).is_err()
                {
                    break;
                }
            }
        }
    }
}

/// Read side of the bridge: parse stdout lines, resolve pending command
/// responses by id, and forward session events. Exits when pi closes stdout;
/// then fails all pending requests, notifies the UI, and reaps the child.
fn run_reader(
    stdout: std::process::ChildStdout,
    event_tx: mpsc::Sender<PiEvent>,
    pending: &Arc<Mutex<HashMap<String, mpsc::Sender<serde_json::Value>>>>,
    mut child: Child,
) {
    let mut reader = BufReader::new(stdout);

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                eprintln!("[pi rpc] pi process exited (stdout closed)");
                break;
            }
            Ok(_) => {}
            Err(err) => {
                eprintln!("[pi rpc] read error: {err}");
                break;
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let value: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[pi rpc] bad json line: {e}");
                continue;
            }
        };

        // Correlate command responses by id.
        if let Some(id) = value.get("id").and_then(|v| v.as_str()) {
            if let Some(tx) = pending.lock().unwrap().remove(id) {
                let _ = tx.send(value.clone());
                continue;
            }
        }

        match serde_json::from_value::<PiEvent>(value.clone()) {
            Ok(event) => {
                let _ = event_tx.send(event);
            }
            Err(e) => {
                eprintln!("[pi rpc] unhandled event: {e}");
            }
        }
    }

    for (_, tx) in pending.lock().unwrap().drain() {
        let _ = tx.send(failure("rpc process exited"));
    }

    let _ = event_tx.send(PiEvent::Disconnected);
    let _ = child.kill();
    let _ = child.wait();
}

fn failure(error: &str) -> serde_json::Value {
    serde_json::json!({ "success": false, "error": error })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_serialize_to_flat_wire_format() {
        let json = serde_json::to_value(PiCommand::Prompt { message: "hi".into() }).unwrap();
        assert_eq!(json, serde_json::json!({"type": "prompt", "message": "hi"}));

        let json = serde_json::to_value(PiCommand::SetModel {
            provider: "nvidia".into(),
            model_id: "nemotron".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type": "set_model", "provider": "nvidia", "modelId": "nemotron"})
        );

        let json = serde_json::to_value(PiCommand::SwitchSession {
            session_path: "/tmp/s.jsonl".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type": "switch_session", "sessionPath": "/tmp/s.jsonl"})
        );
    }

    #[test]
    fn events_parse_from_flat_wire_format() {
        let ev: PiEvent =
            serde_json::from_value(serde_json::json!({"type": "agent_settled", "aborted": false}))
                .unwrap();
        assert!(matches!(ev, PiEvent::AgentSettled { aborted: Some(false) }));

        let ev: PiEvent = serde_json::from_value(
            serde_json::json!({"type": "message_update", "assistantMessageEvent":
                {"type": "text_delta", "contentIndex": 1, "delta": "hello"}}),
        )
        .unwrap();
        assert!(matches!(ev, PiEvent::MessageUpdate { .. }));

        let ev: PiEvent = serde_json::from_value(
            serde_json::json!({"type": "tool_execution_start", "toolCallId": "c1", "toolName": "bash"}),
        )
        .unwrap();
        assert!(matches!(
            ev,
            PiEvent::ToolExecutionStart { tool_call_id: Some(ref id), tool_name: Some(ref n), .. }
                if id == "c1" && n == "bash"
        ));

        let ev: PiEvent = serde_json::from_value(
            serde_json::json!({"type": "extension_ui_request", "id": "x", "method": "notify",
                "message": "hi", "notifyType": "info"}),
        )
        .unwrap();
        match ev {
            PiEvent::ExtensionUiRequest { id, method, extra } => {
                assert_eq!((id.as_str(), method.as_str()), ("x", "notify"));
                assert_eq!(extra.get("message").and_then(|m| m.as_str()), Some("hi"));
            }
            _ => panic!("wrong variant"),
        }

        let ev: PiEvent =
            serde_json::from_value(serde_json::json!({"type": "queue_update", "steering": []}))
                .unwrap();
        assert!(matches!(ev, PiEvent::Other));

        let ev: PiEvent =
            serde_json::from_value(serde_json::json!({"type": "agent_start"})).unwrap();
        assert!(matches!(ev, PiEvent::AgentStart));

        let ev: PiEvent = serde_json::from_value(
            serde_json::json!({"id": "r1", "type": "response", "command": "prompt",
                "success": true, "data": {}}),
        )
        .unwrap();
        assert!(matches!(ev, PiEvent::Response { .. }));
    }

    /// Regression test for the write-starvation deadlock: with the real pi
    /// child idle and silent (all extension startup chatter drained), two
    /// back-to-back commands — the exact sequence `on_send_message` produces —
    /// must both reach pi. The old single-threaded loop blocked in
    /// `read_line` after the first write and the prompt was never delivered,
    /// hanging the UI on "Working…" forever.
    #[test]
    fn commands_reach_idle_pi_without_waiting_for_output() {
        let (client, stream) = spawn().expect("failed to spawn pi RPC bridge");

        // Wait for pi to finish booting (zsh wrapper + extension loading can
        // take a few seconds) by polling get_state until it succeeds.
        let mut booted = false;
        for _ in 0..60 {
            let rx = client.send(PiCommand::GetState);
            if let Ok(v) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
                if v.get("success").and_then(|s| s.as_bool()) == Some(true) {
                    booted = true;
                    break;
                }
            }
        }
        assert!(booted, "pi did not answer get_state within timeout");

        // Drain events and let pi go quiet: once the startup and get_state
        // chatter ends, nothing else will wake a blocking reader.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            while stream.try_event().is_some() {}
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        while stream.try_event().is_some() {}

        // The deadlock trigger: two commands queued while pi is silent.
        let _name_rx = client.send(PiCommand::SetSessionName { name: "bridge test".into() });
        let _prompt_rx = client.send(PiCommand::Prompt {
            message: "Reply with exactly: bridge ok".into(),
        });

        // The prompt must start the agent even though pi produced no output
        // between the two sends.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut started = false;
        let mut settled = false;
        while std::time::Instant::now() < deadline && !settled {
            while let Some(event) = stream.try_event() {
                match event {
                    PiEvent::AgentStart => started = true,
                    PiEvent::AgentSettled { .. } => settled = true,
                    _ => {}
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(started, "agent_start never arrived — prompt starved behind a blocking read");
        assert!(settled, "agent never settled — run did not complete");
    }
}
