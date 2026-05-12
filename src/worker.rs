// Worker process: a stream-json claude child, owned by orc.
//
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
    /// First system event carries claude's session_id — capture for --resume.
    ClaudeSessionId {
        session_id: String,
        claude_session_id: String,
    },
    /// Stream-json child exited.
    Exited {
        session_id: String,
        code: Option<i32>,
    },
    /// Synthetic event: orc sent the worker an instruction (initial task or
    /// follow-up). Surfaced in the worker tab as an "orc → ..." line.
    OrcInstruction { session_id: String, text: String },
    /// Latest assistant turn's context-window occupancy in tokens.
    /// `tokens` = input + cache_read + cache_creation from /message/usage.
    Usage {
        session_id: String,
        context_tokens: u64,
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
        self.send_with(message, &[]).await
    }

    /// Send variant with image content blocks attached. Empty
    /// `attachments` keeps the wire shape identical to `send`.
    pub async fn send_with(
        &self,
        message: &str,
        attachments: &[crate::input_attachments::Attachment],
    ) -> Result<()> {
        let msg = crate::input_attachments::build_user_message(message, attachments);
        let line = serde_json::to_string(&msg)?;
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Cancel an in-flight turn without killing the conversation. Stream-json
    /// has no out-of-band interrupt today, so we send SIGINT to the child;
    /// claude treats it as cancel-current-turn and stays alive.
    pub async fn interrupt(&self) -> Result<()> {
        let child = self.child.lock().await;
        if let Some(pid) = child.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGINT);
            }
        }
        Ok(())
    }

    pub async fn kill(&self) -> Result<()> {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        Ok(())
    }
}

/// Path to the `claude` binary to spawn. Falls back to `claude` on PATH.
/// Tests/harness override via `ORC_CLAUDE_BIN` to a stream-json shim so
/// runs are free and deterministic.
pub fn claude_bin() -> String {
    std::env::var("ORC_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string())
}

fn build_base_command(
    model: &str,
    mcp_config_path: &PathBuf,
    system_prompt: &str,
) -> Command {
    let mut cmd = Command::new(claude_bin());
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--model",
        model,
        "--mcp-config",
        mcp_config_path.to_str().unwrap_or(""),
        "--strict-mcp-config",
        "--dangerously-skip-permissions",
        "--append-system-prompt",
        system_prompt,
    ]);
    // Force claude to auto-compact at 50% — past that the model starts
    // hallucinating in our experience.
    cmd.env("CLAUDE_AUTOCOMPACT_PCT_OVERRIDE", "50");
    cmd
}

/// Context window size for a given model alias. claude doesn't expose this
/// in the stream, so we mirror the published per-model caps.
pub fn context_cap_for(model: &str) -> u64 {
    let m = model.to_ascii_lowercase();
    if m.contains("opus") || m.contains("sonnet-4-6") || m == "sonnet" {
        1_000_000
    } else {
        200_000
    }
}

async fn finalise_spawn(
    session_id: String,
    worktree: PathBuf,
    model: String,
    mut child: Child,
    events_tx: mpsc::UnboundedSender<WorkerEvent>,
) -> Result<WorkerHandle> {
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
        worktree,
        model,
        child: child_arc.clone(),
        stdin: stdin_arc.clone(),
    };

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
            let code = {
                let mut child = child_arc.lock().await;
                child.wait().await.ok().and_then(|s| s.code())
            };
            let _ = events_tx.send(WorkerEvent::Exited { session_id, code });
        });
    }

    Ok(handle)
}

/// Spawn a stream-json claude child as a worker. The task is sent as the
/// first user message after the child is up.
pub async fn spawn_worker(
    session_id: String,
    worktree: PathBuf,
    model: String,
    mcp_config_path: PathBuf,
    system_prompt: String,
    task: String,
    events_tx: mpsc::UnboundedSender<WorkerEvent>,
) -> Result<WorkerHandle> {
    let mut cmd = build_base_command(&model, &mcp_config_path, &system_prompt);
    cmd.current_dir(&worktree);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());

    let child = cmd.spawn()?;
    let handle = finalise_spawn(session_id.clone(), worktree, model, child, events_tx).await?;

    if let Err(e) = handle.send(&task).await {
        eprintln!(
            "[worker {}] failed to deliver initial task: {}",
            session_id, e
        );
    }

    Ok(handle)
}

/// Resume a worker against a captured `claude --resume <id>` session. No
/// initial seed; the user's next message resumes the conversation.
pub async fn spawn_worker_resume(
    session_id: String,
    worktree: PathBuf,
    model: String,
    mcp_config_path: PathBuf,
    system_prompt: String,
    claude_session_id: String,
    events_tx: mpsc::UnboundedSender<WorkerEvent>,
) -> Result<WorkerHandle> {
    let mut cmd = build_base_command(&model, &mcp_config_path, &system_prompt);
    cmd.args(["--resume", &claude_session_id]);
    cmd.current_dir(&worktree);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());

    let child = cmd.spawn()?;
    finalise_spawn(session_id, worktree, model, child, events_tx).await
}

/// Parse a stream-json line into zero or more typed worker events.
pub fn parse_worker_events(session_id: &str, raw: &Value) -> Vec<WorkerEvent> {
    let mut out = Vec::new();
    let event_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match event_type {
        "system" => {
            if let Some(sid) = raw.get("session_id").and_then(|v| v.as_str()) {
                out.push(WorkerEvent::ClaudeSessionId {
                    session_id: session_id.to_string(),
                    claude_session_id: sid.to_string(),
                });
            }
        }
        "assistant" => {
            if let Some(usage) = raw.pointer("/message/usage") {
                let input = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                let cache_read = usage
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let cache_creation = usage
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let total = input + cache_read + cache_creation;
                if total > 0 {
                    out.push(WorkerEvent::Usage {
                        session_id: session_id.to_string(),
                        context_tokens: total,
                    });
                }
            }
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
    fn parse_system_emits_claude_session_id() {
        let raw = serde_json::json!({
            "type": "system",
            "session_id": "abc-123",
            "model": "claude-sonnet-4"
        });
        let events = parse_worker_events("worker-1", &raw);
        assert_eq!(events.len(), 1);
        match &events[0] {
            WorkerEvent::ClaudeSessionId {
                session_id,
                claude_session_id,
            } => {
                assert_eq!(session_id, "worker-1");
                assert_eq!(claude_session_id, "abc-123");
            }
            _ => panic!("expected ClaudeSessionId"),
        }
    }

    #[test]
    fn parse_unknown_event_type_is_empty() {
        let raw = serde_json::json!({ "type": "unknown" });
        assert!(parse_worker_events("s1", &raw).is_empty());
    }
}
