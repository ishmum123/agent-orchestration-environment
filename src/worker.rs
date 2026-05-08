// Worker process: a stream-json claude child, owned by orc.
//
// Mirrors `orc::OrcProcess` but plural — one per spawned worker session.
// Workers communicate via stdin/stdout pipes (no tmux). The user interacts
// with a worker entirely through orc's TUI: a chat-style view that renders
// the structured event stream and lets the user type instructions back. The
// raw claude REPL is never visible — orc IS the interface.

use anyhow::Result;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex};

#[derive(Debug, Clone)]
pub enum WorkerEvent {
    /// Worker emitted assistant text.
    Text { session_id: String, text: String },
    /// Worker is calling an MCP tool (or any tool).
    ToolUse {
        session_id: String,
        name: String,
        input: Value,
    },
    /// Tool returned a result (rendered as a system note).
    ToolResult {
        session_id: String,
        text: String,
        is_error: bool,
    },
    /// Worker emitted a thinking block.
    Thinking { session_id: String, text: String },
    /// Worker finished a turn (tracks cost).
    Result {
        session_id: String,
        is_error: bool,
        cost_usd: Option<f64>,
    },
    /// Stream-json child exited.
    Exited {
        session_id: String,
        code: Option<i32>,
    },
}

pub struct WorkerHandle {
    pub session_id: String,
    pub worktree: PathBuf,
    pub model: String,
    child: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<ChildStdin>>,
}

impl WorkerHandle {
    /// Send a user message to the worker (multi-turn instruction).
    pub async fn send(&self, message: &str) -> Result<()> {
        let msg = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": message }
        });
        let line = serde_json::to_string(&msg)?;
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    pub async fn kill(&self) -> Result<()> {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        Ok(())
    }
}

/// Spawn a stream-json claude child as a worker. The task is sent as the
/// first user message after the child is up. A tokio task drains stdout,
/// parses each NDJSON line, and forwards `WorkerEvent`s to `events_tx` until
/// EOF or kill.
pub async fn spawn_worker(
    session_id: String,
    worktree: PathBuf,
    model: String,
    mcp_config_path: PathBuf,
    system_prompt: String,
    task: String,
    events_tx: mpsc::UnboundedSender<WorkerEvent>,
) -> Result<WorkerHandle> {
    let mut cmd = Command::new("claude");
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--model",
        &model,
        "--mcp-config",
        mcp_config_path.to_str().unwrap_or(""),
        "--strict-mcp-config",
        "--dangerously-skip-permissions",
        "--append-system-prompt",
        &system_prompt,
    ]);
    cmd.current_dir(&worktree);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());

    let mut child = cmd.spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture worker stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture worker stdout"))?;

    let child_arc = Arc::new(Mutex::new(child));
    let stdin_arc = Arc::new(Mutex::new(stdin));

    let handle = WorkerHandle {
        session_id: session_id.clone(),
        worktree: worktree.clone(),
        model: model.clone(),
        child: child_arc.clone(),
        stdin: stdin_arc.clone(),
    };

    // Spawn the stdout reader task.
    {
        let session_id = session_id.clone();
        let events_tx = events_tx.clone();
        let child_arc = child_arc.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let raw: Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                for ev in parse_worker_events(&session_id, &raw) {
                    if events_tx.send(ev).is_err() {
                        return;
                    }
                }
            }
            // Stream closed — wait on the child to record exit code.
            let code = {
                let mut child = child_arc.lock().await;
                child.wait().await.ok().and_then(|s| s.code())
            };
            let _ = events_tx.send(WorkerEvent::Exited { session_id, code });
        });
    }

    // Seed the conversation with the task as the first user message.
    if let Err(e) = handle.send(&task).await {
        eprintln!(
            "[worker {}] failed to deliver initial task: {}",
            session_id, e
        );
    }

    Ok(handle)
}

/// Parse a stream-json line into zero or more typed worker events.
pub fn parse_worker_events(session_id: &str, raw: &Value) -> Vec<WorkerEvent> {
    let mut out = Vec::new();
    let event_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match event_type {
        "assistant" => {
            if let Some(content) = raw.pointer("/message/content").and_then(|v| v.as_array()) {
                for block in content {
                    let bt = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match bt {
                        "text" => {
                            if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                                out.push(WorkerEvent::Text {
                                    session_id: session_id.to_string(),
                                    text: t.to_string(),
                                });
                            }
                        }
                        "tool_use" => {
                            let name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let input = block.get("input").cloned().unwrap_or(Value::Null);
                            out.push(WorkerEvent::ToolUse {
                                session_id: session_id.to_string(),
                                name,
                                input,
                            });
                        }
                        "thinking" => {
                            if let Some(t) = block.get("thinking").and_then(|v| v.as_str()) {
                                out.push(WorkerEvent::Thinking {
                                    session_id: session_id.to_string(),
                                    text: t.to_string(),
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        "user" => {
            // Tool results come back as a "user" message containing a
            // tool_result block (claude convention). Surface as ToolResult.
            if let Some(content) = raw.pointer("/message/content").and_then(|v| v.as_array()) {
                for block in content {
                    if block.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
                        let is_error = block
                            .get("is_error")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let text = block
                            .get("content")
                            .and_then(|v| {
                                if let Some(s) = v.as_str() {
                                    Some(s.to_string())
                                } else if let Some(arr) = v.as_array() {
                                    Some(
                                        arr.iter()
                                            .filter_map(|b| {
                                                b.get("text").and_then(|t| t.as_str())
                                            })
                                            .collect::<Vec<_>>()
                                            .join("\n"),
                                    )
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_default();
                        out.push(WorkerEvent::ToolResult {
                            session_id: session_id.to_string(),
                            text,
                            is_error,
                        });
                    }
                }
            }
        }
        "result" => {
            let is_error = raw.get("subtype").and_then(|v| v.as_str()) == Some("error");
            let cost_usd = raw.get("total_cost_usd").and_then(|v| v.as_f64());
            out.push(WorkerEvent::Result {
                session_id: session_id.to_string(),
                is_error,
                cost_usd,
            });
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_assistant_text_and_tool_use() {
        let raw = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    { "type": "text", "text": "Looking into it" },
                    { "type": "tool_use", "name": "Read", "input": { "file": "src/main.rs" } }
                ]
            }
        });
        let events = parse_worker_events("s1", &raw);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], WorkerEvent::Text { .. }));
        assert!(matches!(events[1], WorkerEvent::ToolUse { .. }));
    }

    #[test]
    fn parse_tool_result_string() {
        let raw = serde_json::json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "is_error": false,
                        "content": "file written"
                    }
                ]
            }
        });
        let events = parse_worker_events("s1", &raw);
        assert_eq!(events.len(), 1);
        match &events[0] {
            WorkerEvent::ToolResult {
                text, is_error, ..
            } => {
                assert_eq!(text, "file written");
                assert!(!is_error);
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn parse_tool_result_block_array() {
        let raw = serde_json::json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "is_error": true,
                        "content": [
                            { "type": "text", "text": "denied: write outside worktree" }
                        ]
                    }
                ]
            }
        });
        let events = parse_worker_events("s1", &raw);
        assert_eq!(events.len(), 1);
        match &events[0] {
            WorkerEvent::ToolResult {
                text, is_error, ..
            } => {
                assert_eq!(text, "denied: write outside worktree");
                assert!(*is_error);
            }
            _ => panic!("expected ToolResult"),
        }
    }

    #[test]
    fn parse_result_event() {
        let raw = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "total_cost_usd": 0.012
        });
        let events = parse_worker_events("s1", &raw);
        assert_eq!(events.len(), 1);
        match &events[0] {
            WorkerEvent::Result {
                is_error, cost_usd, ..
            } => {
                assert!(!is_error);
                assert_eq!(*cost_usd, Some(0.012));
            }
            _ => panic!("expected Result"),
        }
    }

    #[test]
    fn parse_unknown_event_type_is_empty() {
        let raw = serde_json::json!({ "type": "unknown" });
        assert!(parse_worker_events("s1", &raw).is_empty());
    }
}
