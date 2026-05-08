// Unix socket hook receiver: listen for agent lifecycle events.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEvent {
    pub session_id: String,
    #[serde(flatten)]
    pub kind: HookKind,
    #[serde(default)]
    pub timestamp: Option<String>, // ISO 8601, injected by relay script
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "hook")]
pub enum HookKind {
    PreToolUse {
        tool_name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    PostToolUse {
        tool_name: String,
        #[serde(default)]
        input: serde_json::Value,
        #[serde(default)]
        output: serde_json::Value,
    },
    Stop {
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        result: Option<String>,
    },
    Notification {
        #[serde(default)]
        message: Option<String>,
    },
}

pub struct HookServer {
    socket_path: PathBuf,
    listener: UnixListener,
    event_tx: mpsc::Sender<HookEvent>,
}

impl HookServer {
    /// Bind to a Unix socket at the given path.
    /// Removes an existing socket file if present.
    pub async fn bind(path: &Path, event_tx: mpsc::Sender<HookEvent>) -> Result<Self> {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        Ok(Self {
            socket_path: path.to_path_buf(),
            listener,
            event_tx,
        })
    }

    /// Run the server loop. Accepts connections, reads NDJSON, sends parsed events to channel.
    /// Each connection can send multiple events (one per line).
    /// This runs forever — spawn as a tokio task.
    pub async fn run(self) -> Result<()> {
        loop {
            let (stream, _addr) = self.listener.accept().await?;
            let tx = self.event_tx.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stream);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<HookEvent>(&line) {
                        Ok(event) => {
                            if tx.send(event).await.is_err() {
                                // Receiver dropped; stop processing
                                break;
                            }
                        }
                        Err(e) => {
                            eprintln!("[hooks] parse warning: {e} — line: {line}");
                        }
                    }
                }
            });
        }
    }

    /// Get the socket path (for configuring workers).
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

/// Generate Claude Code hooks configuration that sends events to our socket.
/// `relay_script` is the path to the relay script created by `create_hook_script()`.
/// Returns JSON suitable for use as a Claude Code hooks config.
///
/// Claude Code expects hooks config in the format:
/// ```json
/// { "hooks": { "PreToolUse": [{ "hooks": [{ "type": "command", "command": "..." }] }] } }
/// ```
pub fn generate_hooks_config(relay_script: &Path) -> String {
    let script = relay_script.to_string_lossy();
    let make_entry = || -> serde_json::Value {
        serde_json::json!([{
            "hooks": [{
                "type": "command",
                "command": format!("python3 {}", script)
            }]
        }])
    };

    let config = serde_json::json!({
        "hooks": {
            "PreToolUse":  make_entry(),
            "PostToolUse": make_entry(),
            "Stop":        make_entry(),
            "Notification": make_entry(),
        }
    });
    serde_json::to_string_pretty(&config).unwrap()
}

/// Create a hook relay script for a worker session.
/// The script reads Claude Code's hook JSON from stdin, maps field names to our
/// internal format, adds session_id + timestamp, and sends to the Unix socket.
/// Returns the path to the script.
///
/// Claude Code sends: hook_event_name, tool_name, tool_input, tool_response,
///   stop_hook_active, last_assistant_message, message, title, notification_type
/// We expect: hook, tool_name, input, output, reason, result, message
pub async fn create_hook_script(socket_path: &Path, session_id: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("orc_hooks");
    tokio::fs::create_dir_all(&dir).await?;

    let script_path = dir.join(format!("relay_{session_id}.py"));
    let sock = socket_path.to_string_lossy();

    let script = format!(
        r#"#!/usr/bin/env python3
import sys, json, socket
from datetime import datetime, timezone

payload = json.load(sys.stdin)

# Map Claude Code field names to our internal format
out = {{}}
out["session_id"] = "{session_id}"
out["timestamp"] = datetime.now(timezone.utc).isoformat()

# Map hook_event_name -> hook
hook_type = payload.get("hook_event_name", payload.get("hook", ""))
out["hook"] = hook_type
out["tool_name"] = payload.get("tool_name", "")

if hook_type == "PreToolUse":
    out["input"] = payload.get("tool_input", payload.get("input", {{}}))
elif hook_type == "PostToolUse":
    out["input"] = payload.get("tool_input", payload.get("input", {{}}))
    out["output"] = payload.get("tool_response", payload.get("output", {{}}))
elif hook_type == "Stop":
    out["reason"] = payload.get("stop_hook_active", payload.get("reason"))
    out["result"] = payload.get("last_assistant_message", payload.get("result"))
elif hook_type == "Notification":
    out["message"] = payload.get("message")

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect("{sock}")
sock.sendall((json.dumps(out) + "\n").encode())
sock.close()
"#
    );

    tokio::fs::write(&script_path, script).await?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms)?;
    }

    Ok(script_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;

    #[tokio::test]
    async fn hook_event_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("hooks.sock");
        let (tx, mut rx) = mpsc::channel(32);

        let server = HookServer::bind(&sock, tx).await.unwrap();
        tokio::spawn(server.run());

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut stream = UnixStream::connect(&sock).await.unwrap();
        let payload = r#"{"session_id":"test-1","hook":"PostToolUse","tool_name":"Read","input":{"file":"src/main.rs"},"output":{}}"#;
        stream.write_all(payload.as_bytes()).await.unwrap();
        stream.write_all(b"\n").await.unwrap();
        stream.shutdown().await.unwrap();

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.session_id, "test-1");
        assert!(matches!(event.kind, HookKind::PostToolUse { .. }));
        assert!(event.timestamp.is_none()); // not provided in this payload
    }

    #[tokio::test]
    async fn multiple_events_on_single_connection() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("hooks_multi.sock");
        let (tx, mut rx) = mpsc::channel(32);

        let server = HookServer::bind(&sock, tx).await.unwrap();
        tokio::spawn(server.run());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut stream = UnixStream::connect(&sock).await.unwrap();
        for i in 0..3 {
            let payload = format!(
                r#"{{"session_id":"s{i}","hook":"Notification","message":"msg{i}"}}"#
            );
            stream.write_all(payload.as_bytes()).await.unwrap();
            stream.write_all(b"\n").await.unwrap();
        }
        stream.shutdown().await.unwrap();

        for i in 0..3 {
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(event.session_id, format!("s{i}"));
        }
    }

    #[tokio::test]
    async fn malformed_json_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("hooks_malformed.sock");
        let (tx, mut rx) = mpsc::channel(32);

        let server = HookServer::bind(&sock, tx).await.unwrap();
        tokio::spawn(server.run());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut stream = UnixStream::connect(&sock).await.unwrap();
        stream.write_all(b"not valid json\n").await.unwrap();
        let valid = r#"{"session_id":"good","hook":"Stop"}"#;
        stream.write_all(valid.as_bytes()).await.unwrap();
        stream.write_all(b"\n").await.unwrap();
        stream.shutdown().await.unwrap();

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.session_id, "good");
        assert!(matches!(event.kind, HookKind::Stop { .. }));
    }

    #[tokio::test]
    async fn stop_event_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("hooks_stop.sock");
        let (tx, mut rx) = mpsc::channel(32);

        let server = HookServer::bind(&sock, tx).await.unwrap();
        tokio::spawn(server.run());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut stream = UnixStream::connect(&sock).await.unwrap();
        let payload = r#"{"session_id":"sess-stop","hook":"Stop","reason":"completed","result":"success","timestamp":"2025-06-01T00:00:00Z"}"#;
        stream.write_all(payload.as_bytes()).await.unwrap();
        stream.write_all(b"\n").await.unwrap();
        stream.shutdown().await.unwrap();

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.session_id, "sess-stop");
        assert_eq!(event.timestamp.as_deref(), Some("2025-06-01T00:00:00Z"));
        match event.kind {
            HookKind::Stop { reason, result } => {
                assert_eq!(reason.as_deref(), Some("completed"));
                assert_eq!(result.as_deref(), Some("success"));
            }
            _ => panic!("expected Stop"),
        }
    }

    #[tokio::test]
    async fn relay_script_maps_claude_code_fields() {
        // Test that the relay script correctly maps Claude Code's field names
        // (hook_event_name, tool_input, tool_response) to our internal format.
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("relay_map.sock");
        let (tx, mut rx) = mpsc::channel(32);

        let server = HookServer::bind(&sock_path, tx).await.unwrap();
        tokio::spawn(server.run());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let script = create_hook_script(&sock_path, "map-test").await.unwrap();

        // Send a payload using Claude Code's ACTUAL field names
        let claude_payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "echo hello"},
            "session_id": "claude-internal-id",
            "cwd": "/some/path",
            "permission_mode": "default"
        });

        let mut child = tokio::process::Command::new("python3")
            .arg(&script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("python3 must be available");

        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(serde_json::to_string(&claude_payload).unwrap().as_bytes())
            .await
            .unwrap();
        drop(stdin);

        let status = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            child.wait(),
        )
        .await
        .expect("timed out")
        .expect("wait failed");
        assert!(status.success(), "relay script failed");

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();

        // Verify the relay script mapped fields correctly
        assert_eq!(event.session_id, "map-test"); // our session_id, not Claude's
        assert!(event.timestamp.is_some()); // injected by relay
        match &event.kind {
            HookKind::PreToolUse { tool_name, input } => {
                assert_eq!(tool_name, "Bash");
                assert_eq!(input["command"], "echo hello");
            }
            _ => panic!("expected PreToolUse, got {:?}", event.kind),
        }

        let _ = tokio::fs::remove_file(&script).await;
    }

    #[tokio::test]
    async fn relay_script_maps_post_tool_use() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("relay_post.sock");
        let (tx, mut rx) = mpsc::channel(32);

        let server = HookServer::bind(&sock_path, tx).await.unwrap();
        tokio::spawn(server.run());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let script = create_hook_script(&sock_path, "post-test").await.unwrap();

        // Claude Code PostToolUse uses tool_input and tool_response
        let claude_payload = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Read",
            "tool_input": {"file_path": "/src/main.rs"},
            "tool_response": {"content": "fn main() {}"}
        });

        let mut child = tokio::process::Command::new("python3")
            .arg(&script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(serde_json::to_string(&claude_payload).unwrap().as_bytes())
            .await
            .unwrap();
        drop(stdin);
        let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(event.session_id, "post-test");
        match &event.kind {
            HookKind::PostToolUse {
                tool_name,
                input,
                output,
            } => {
                assert_eq!(tool_name, "Read");
                assert_eq!(input["file_path"], "/src/main.rs");
                assert_eq!(output["content"], "fn main() {}");
            }
            _ => panic!("expected PostToolUse, got {:?}", event.kind),
        }

        let _ = tokio::fs::remove_file(&script).await;
    }

    #[tokio::test]
    async fn relay_script_maps_stop_event() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("relay_stop.sock");
        let (tx, mut rx) = mpsc::channel(32);

        let server = HookServer::bind(&sock_path, tx).await.unwrap();
        tokio::spawn(server.run());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let script = create_hook_script(&sock_path, "stop-test").await.unwrap();

        // Claude Code Stop hook uses stop_hook_active and last_assistant_message
        let claude_payload = serde_json::json!({
            "hook_event_name": "Stop",
            "stop_hook_active": "task_complete",
            "last_assistant_message": "All done!"
        });

        let mut child = tokio::process::Command::new("python3")
            .arg(&script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(serde_json::to_string(&claude_payload).unwrap().as_bytes())
            .await
            .unwrap();
        drop(stdin);
        let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(event.session_id, "stop-test");
        match &event.kind {
            HookKind::Stop { reason, result } => {
                assert_eq!(reason.as_deref(), Some("task_complete"));
                assert_eq!(result.as_deref(), Some("All done!"));
            }
            _ => panic!("expected Stop, got {:?}", event.kind),
        }

        let _ = tokio::fs::remove_file(&script).await;
    }

    #[test]
    fn generate_hooks_config_structure() {
        let script_path = std::path::PathBuf::from("/tmp/orc_hooks/relay_test.py");
        let config_str = generate_hooks_config(&script_path);
        let config: serde_json::Value = serde_json::from_str(&config_str).unwrap();

        // Verify correct nested structure: hooks.PreToolUse[0].hooks[0].type
        let pre = &config["hooks"]["PreToolUse"];
        assert!(pre.is_array());
        assert_eq!(pre[0]["hooks"][0]["type"], "command");
        assert!(pre[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("relay_test.py"));

        // All four hook types present
        for hook_type in &["PreToolUse", "PostToolUse", "Stop", "Notification"] {
            assert!(
                config["hooks"][hook_type].is_array(),
                "missing hook type: {}",
                hook_type
            );
        }
    }
}
