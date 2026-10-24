//! Non-blocking RPC bridge to the pi coding agent.
//!
//! A dedicated background thread owns the `pi --mode rpc` child process. The UI
//! thread sends commands through a channel and receives responses through
//! per-request oneshot channels. Async events are forwarded to a channel the
//! UI thread polls via `glib::timeout_add`. This keeps the GTK main loop
//! responsive.
//!
//! Wire format (see pi's docs/rpc-commands.md and docs/json.md): commands and
//! events are flat JSON objects with a `"type"` discriminator, e.g.
//! `{"type":"prompt","message":"hi"}` — NOT type+payload envelopes.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

/// A command sent to the pi agent. Field names serialize to camelCase per the
/// wire protocol (`modelId`, `sessionPath`, …).
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PiCommand {
    Prompt { message: String },
    Steer { message: String },
    Abort,
    NewSession {
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_session: Option<String>,
    },
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
    ToolExecutionStart { tool_call_id: Option<String>, tool_name: Option<String> },
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

/// Spawn the RPC background thread.
pub fn spawn() -> Result<(PiRpcClient, PiEventStream), Box<dyn std::error::Error + Send + Sync>> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<RpcMsg>();
    let (event_tx, event_rx) = mpsc::channel::<PiEvent>();

    let client = PiRpcClient {
        tx: cmd_tx,
        next_id: std::sync::atomic::AtomicU64::new(0),
    };
    let stream = PiEventStream { rx: event_rx };

    let cli_path = resolve_pi_path();
    thread::spawn(move || {
        if let Err(err) = run_rpc_loop(&cli_path, cmd_rx, event_tx) {
            eprintln!("[pi rpc] loop error: {err}");
        }
    });

    Ok((client, stream))
}

fn run_rpc_loop(
    cli_path: &str,
    cmd_rx: mpsc::Receiver<RpcMsg>,
    event_tx: mpsc::Sender<PiEvent>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut child = spawn_pi(cli_path)?;

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

    let mut reader = BufReader::new(stdout);

    // Pending request id -> response sender.
    let mut pending: HashMap<String, mpsc::Sender<serde_json::Value>> = HashMap::new();

    loop {
        let cmd = cmd_rx.try_recv().ok();

        match cmd {
            Some(RpcMsg::Command { id, command, tx }) => {
                let mut value = serde_json::to_value(&command)
                    .map_err(|e| format!("failed to serialize command: {e}"))?;
                if let Some(map) = value.as_object_mut() {
                    map.insert("id".into(), serde_json::json!(id));
                }
                let line = serde_json::to_string(&value)
                    .map_err(|e| format!("failed to serialize command: {e}"))?;
                if let Err(err) = stdin.write_all(format!("{line}\n").as_bytes()) {
                    let _ = tx.send(serde_json::json!({
                        "success": false, "error": format!("failed to write to pi: {err}")
                    }));
                    break;
                }
                let _ = stdin.flush();
                pending.insert(id, tx);
            }
            Some(RpcMsg::ExtensionResponse { id, mut value }) => {
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
                let _ = stdin.write_all(format!("{line}\n").as_bytes());
                let _ = stdin.flush();
            }
            None => {}
        }

        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                eprintln!("[pi rpc] pi process exited (stdout closed)");
                let _ = event_tx.send(PiEvent::Disconnected);
                break;
            }
            Ok(_) => {}
            Err(err) => {
                eprintln!("[pi rpc] read error: {err}");
                let _ = event_tx.send(PiEvent::Disconnected);
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
            if let Some(tx) = pending.remove(id) {
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

    for (_, tx) in pending.drain() {
        let _ = tx.send(serde_json::json!({ "success": false, "error": "rpc process exited" }));
    }

    let _ = child.kill();
    Ok(())
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

        let json = serde_json::to_value(PiCommand::NewSession { parent_session: None }).unwrap();
        assert_eq!(json, serde_json::json!({"type": "new_session"}));

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
            PiEvent::ToolExecutionStart { tool_call_id: Some(ref id), tool_name: Some(ref n) }
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
}
