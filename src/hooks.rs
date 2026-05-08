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
    pub timestamp: String, // ISO 8601
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
/// Returns JSON string for the hooks section of .claude/settings.json
pub fn generate_hooks_config(socket_path: &Path, session_id: &str) -> String {
    let sock = socket_path.to_string_lossy();
    // Each hook entry runs the relay script via bash.
    let make_entry = |hook_type: &str| -> serde_json::Value {
        serde_json::json!([{
            "type": "command",
            "command": format!(
                "bash -c 'python3 -c \"import sys,json,socket,os; \
                 payload=json.load(sys.stdin); \
                 payload[\\\"session_id\\\"]=\\\"{session_id}\\\"; \
                 payload[\\\"hook\\\"]=\\\"{hook_type}\\\"; \
                 s=socket.socket(socket.AF_UNIX); \
                 s.connect(\\\"{sock}\\\"); \
                 s.sendall((json.dumps(payload)+\\\"\\\\n\\\").encode()); \
                 s.close()\"'"
            )
        }])
    };

    let config = serde_json::json!({
        "hooks": {
            "PreToolUse":  make_entry("PreToolUse"),
            "PostToolUse": make_entry("PostToolUse"),
            "Stop":        make_entry("Stop"),
            "Notification": make_entry("Notification"),
        }
    });
    serde_json::to_string_pretty(&config).unwrap()
}

/// Create a hook relay script for a worker session.
/// The script reads Claude Code's hook JSON from stdin, wraps it with session_id,
/// and sends to the Unix socket.
/// Returns the path to the script.
pub async fn create_hook_script(socket_path: &Path, session_id: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("orc_hooks");
    tokio::fs::create_dir_all(&dir).await?;

    let script_path = dir.join(format!("relay_{session_id}.py"));
    let sock = socket_path.to_string_lossy();

    let script = format!(
        r#"#!/usr/bin/env python3
import sys, json, socket

payload = json.load(sys.stdin)
payload["session_id"] = "{session_id}"

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect("{sock}")
sock.sendall((json.dumps(payload) + "\n").encode())
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
        let payload = r#"{"session_id":"test-1","hook":"PostToolUse","tool_name":"Read","input":{"file":"src/main.rs"},"output":{},"timestamp":"2025-01-01T00:00:00Z"}"#;
        stream.write_all(payload.as_bytes()).await.unwrap();
        stream.write_all(b"\n").await.unwrap();
        stream.shutdown().await.unwrap();

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.session_id, "test-1");
        assert!(matches!(event.kind, HookKind::PostToolUse { .. }));
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
                r#"{{"session_id":"s{i}","hook":"Notification","message":"msg{i}","timestamp":"2025-01-01T00:00:00Z"}}"#
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
        let valid = r#"{"session_id":"good","hook":"Stop","timestamp":"2025-01-01T00:00:00Z"}"#;
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
        let payload = r#"{"session_id":"sess-stop","hook":"Stop","reason":"completed","result":"success","timestamp":"2025-01-01T00:00:00Z"}"#;
        stream.write_all(payload.as_bytes()).await.unwrap();
        stream.write_all(b"\n").await.unwrap();
        stream.shutdown().await.unwrap();

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.session_id, "sess-stop");
        match event.kind {
            HookKind::Stop { reason, result } => {
                assert_eq!(reason.as_deref(), Some("completed"));
                assert_eq!(result.as_deref(), Some("success"));
            }
            _ => panic!("expected Stop"),
        }
    }
}
