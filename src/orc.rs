// Orchestrator brain: spawn and manage the planning agent.

use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// Parsed events from the orc brain's stream-json output.
#[derive(Debug, Clone)]
pub enum OrcEvent {
    /// Orc is producing text (display in chat pane)
    Text(String),
    /// Orc is calling an MCP tool
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// Orc finished a turn
    Result {
        is_error: bool,
        result: String,
        cost_usd: Option<f64>,
        duration_ms: Option<u64>,
    },
    /// System event (init, model info)
    System {
        model: Option<String>,
        session_id: Option<String>,
    },
    /// Thinking/reasoning block
    Thinking(String),
}

/// Parse a raw stream-json Value into zero or more OrcEvents.
pub fn parse_orc_events(raw: &Value) -> Vec<OrcEvent> {
    let mut events = Vec::new();

    let event_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match event_type {
        "system" => {
            events.push(OrcEvent::System {
                model: raw.get("model").and_then(|v| v.as_str()).map(|s| s.to_string()),
                session_id: raw
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            });
        }
        "assistant" => {
            if let Some(content) = raw
                .pointer("/message/content")
                .and_then(|v| v.as_array())
            {
                for block in content {
                    let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match block_type {
                        "text" => {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                events.push(OrcEvent::Text(text.to_string()));
                            }
                        }
                        "tool_use" => {
                            let id = block
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let input = block.get("input").cloned().unwrap_or(Value::Null);
                            events.push(OrcEvent::ToolUse { id, name, input });
                        }
                        "thinking" => {
                            if let Some(text) = block.get("thinking").and_then(|v| v.as_str()) {
                                events.push(OrcEvent::Thinking(text.to_string()));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        "result" => {
            let subtype = raw.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
            let is_error = subtype == "error";
            let result_text = raw
                .get("result")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let cost_usd = raw.get("total_cost_usd").and_then(|v| v.as_f64());
            let duration_ms = raw.get("duration_ms").and_then(|v| v.as_u64());
            events.push(OrcEvent::Result {
                is_error,
                result: result_text,
                cost_usd,
                duration_ms,
            });
        }
        _ => {}
    }

    events
}

/// Tracks cumulative token usage and cost for the orc brain.
#[derive(Debug, Clone, Default)]
pub struct OrcUsage {
    pub total_input: u64,
    pub total_output: u64,
    pub total_cost: f64,
    pub turns: u32,
}

/// Configuration for spawning the orc brain.
pub struct OrcConfig {
    pub project_dir: PathBuf,
    pub mcp_config_path: PathBuf,
    pub model: String, // default "opus" for orc
}

/// A running orc brain process.
pub struct OrcProcess {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    usage: OrcUsage,
}

impl OrcProcess {
    /// Send a user message to the orc brain.
    pub async fn send(&mut self, message: &str) -> Result<()> {
        let msg = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": message
            }
        });
        let line = serde_json::to_string(&msg)?;
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// Try to read the next raw JSON event from the orc brain.
    /// Returns None if no data is available yet or on timeout.
    pub async fn try_read_raw(&mut self) -> Result<Option<Value>> {
        let mut line = String::new();
        match tokio::time::timeout(
            std::time::Duration::from_millis(10),
            self.reader.read_line(&mut line),
        )
        .await
        {
            Ok(Ok(0)) => Ok(None), // EOF
            Ok(Ok(_)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return Ok(None);
                }
                match serde_json::from_str(trimmed) {
                    Ok(v) => Ok(Some(v)),
                    Err(_) => Ok(None), // skip non-JSON lines
                }
            }
            Ok(Err(e)) => Err(e.into()),
            Err(_) => Ok(None), // timeout, no data
        }
    }

    /// Read and parse the next batch of typed events from the orc brain.
    /// Returns an empty vec if no data is available.
    /// Automatically tracks usage from Result events.
    pub async fn read_events(&mut self) -> Result<Vec<OrcEvent>> {
        match self.try_read_raw().await? {
            Some(raw) => {
                let events = parse_orc_events(&raw);
                // Track usage from Result events
                for event in &events {
                    if let OrcEvent::Result { cost_usd, .. } = event {
                        self.usage.turns += 1;
                        if let Some(cost) = cost_usd {
                            self.usage.total_cost = *cost; // total_cost_usd is cumulative
                        }
                    }
                }
                // Track token counts from assistant messages
                if let Some(usage) = raw.pointer("/message/usage") {
                    if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                        self.usage.total_input += input;
                    }
                    if let Some(output) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                        self.usage.total_output += output;
                    }
                }
                Ok(events)
            }
            None => Ok(vec![]),
        }
    }

    /// Get current cumulative usage stats.
    pub fn usage(&self) -> &OrcUsage {
        &self.usage
    }

    /// Check if the process is still running.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Kill the orc process.
    pub async fn kill(&mut self) -> Result<()> {
        self.child.kill().await?;
        Ok(())
    }
}

/// Generate the system prompt for the orc brain.
pub fn system_prompt(project_dir: &Path) -> String {
    format!(
        r#"You are **orc**, an orchestration agent. You manage worker sessions to accomplish the user's goals.

## Your Role

You plan tasks, delegate to worker sessions, monitor progress, and report results. You do NOT write code yourself — you coordinate workers that do.

## Available Tools

You have MCP tools to manage worker sessions:

- **spawn_session(name, task, model?)**: Create a new worker with its own git worktree. Default model is "sonnet". Use "opus" for complex tasks requiring deep reasoning. Use "haiku" for simple lookups.
- **instruct_session(session_id, message)**: Send instructions to a running worker.
- **kill_session(session_id)**: Kill a worker and clean up resources.
- **ask_user(question, context?)**: Ask the user a question. This BLOCKS until they respond. Use sparingly.
- **list_sessions()**: Check status of all workers. Call this before making decisions.
- **mark_done(session_id, summary)**: Mark a worker's task as complete.
- **update_task_graph(graph)**: Update the task plan.

## Workflow

1. When the user gives you a task, break it into subtasks.
2. Call update_task_graph with your plan.
3. Spawn workers for independent subtasks (they can run in parallel).
4. Monitor progress via list_sessions.
5. When a worker finishes, review its work and mark_done.
6. If a worker is stuck, instruct it or kill and respawn.
7. Report results to the user.

## Rules

- Always plan before spawning. Call update_task_graph first.
- Use list_sessions before deciding anything. Don't assume state.
- Name sessions descriptively (e.g., "auth-fix", "add-tests", "refactor-api").
- Default to "sonnet" model for workers. Use "opus" only for hard reasoning tasks.
- If you're unsure what the user wants, call ask_user.
- Never spawn more than 5 workers simultaneously.
- When all tasks are done, summarize the results to the user.
- Keep your responses concise.

## Project

Working directory: {project_dir}
"#,
        project_dir = project_dir.display()
    )
}

/// Generate MCP config file for the orc brain (HTTP transport).
/// The MCP server must already be running on the given port.
/// Returns the path to the generated config file.
pub async fn write_mcp_config(port: u16) -> Result<PathBuf> {
    let config = crate::mcp::generate_mcp_config(port);
    let config_path = std::env::temp_dir().join("orc-mcp-config.json");
    tokio::fs::write(&config_path, serde_json::to_string_pretty(&config)?).await?;
    Ok(config_path)
}

/// Spawn the orc brain as a Claude Code process.
pub async fn spawn_orc(config: &OrcConfig) -> Result<OrcProcess> {
    let prompt = system_prompt(&config.project_dir);

    let mut cmd = Command::new("claude");
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--model",
        &config.model,
        "--system-prompt",
        &prompt,
        "--mcp-config",
        config.mcp_config_path.to_str().unwrap_or(""),
        "--strict-mcp-config",
        "--dangerously-skip-permissions",
    ]);
    cmd.current_dir(&config.project_dir);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());

    let mut child = cmd.spawn()?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture stdout"))?;
    let reader = BufReader::new(stdout);

    Ok(OrcProcess {
        child,
        stdin,
        reader,
        usage: OrcUsage::default(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_contains_key_elements() {
        let prompt = system_prompt(Path::new("/tmp/test-project"));
        assert!(prompt.contains("orchestration agent"));
        assert!(prompt.contains("spawn_session"));
        assert!(prompt.contains("instruct_session"));
        assert!(prompt.contains("kill_session"));
        assert!(prompt.contains("ask_user"));
        assert!(prompt.contains("list_sessions"));
        assert!(prompt.contains("mark_done"));
        assert!(prompt.contains("update_task_graph"));
        assert!(prompt.contains("/tmp/test-project"));
    }

    #[test]
    fn system_prompt_has_rules() {
        let prompt = system_prompt(Path::new("/tmp/test"));
        assert!(prompt.contains("plan before spawning"));
        assert!(prompt.contains("list_sessions before deciding"));
        assert!(prompt.contains("5 workers"));
    }

    #[tokio::test]
    async fn mcp_config_written_http() {
        let path = write_mcp_config(9999).await.unwrap();
        assert!(path.exists());
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mcpServers"]["orc"]["type"], "http");
        assert!(parsed["mcpServers"]["orc"]["url"]
            .as_str()
            .unwrap()
            .contains("9999"));
        tokio::fs::remove_file(&path).await.ok();
    }

    #[test]
    fn parse_system_event() {
        let raw = serde_json::json!({
            "type": "system",
            "subtype": "init",
            "session_id": "sess-123",
            "model": "claude-sonnet-4-20250514"
        });
        let events = parse_orc_events(&raw);
        assert_eq!(events.len(), 1);
        match &events[0] {
            OrcEvent::System { model, session_id } => {
                assert_eq!(model.as_deref(), Some("claude-sonnet-4-20250514"));
                assert_eq!(session_id.as_deref(), Some("sess-123"));
            }
            other => panic!("expected System, got {:?}", other),
        }
    }

    #[test]
    fn parse_assistant_text_and_tool_use() {
        let raw = serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "I'll spawn a worker."},
                    {"type": "tool_use", "id": "toolu_1", "name": "spawn_session", "input": {"name": "auth", "task": "fix auth"}}
                ],
                "usage": {"input_tokens": 100, "output_tokens": 50}
            }
        });
        let events = parse_orc_events(&raw);
        assert_eq!(events.len(), 2);
        match &events[0] {
            OrcEvent::Text(t) => assert_eq!(t, "I'll spawn a worker."),
            other => panic!("expected Text, got {:?}", other),
        }
        match &events[1] {
            OrcEvent::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_1");
                assert_eq!(name, "spawn_session");
                assert_eq!(input["name"], "auth");
            }
            other => panic!("expected ToolUse, got {:?}", other),
        }
    }

    #[test]
    fn parse_assistant_thinking() {
        let raw = serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "Let me reason about this..."}
                ]
            }
        });
        let events = parse_orc_events(&raw);
        assert_eq!(events.len(), 1);
        match &events[0] {
            OrcEvent::Thinking(t) => assert_eq!(t, "Let me reason about this..."),
            other => panic!("expected Thinking, got {:?}", other),
        }
    }

    #[test]
    fn parse_result_success() {
        let raw = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "result": "done",
            "session_id": "sess-123",
            "duration_ms": 5000,
            "total_cost_usd": 0.05
        });
        let events = parse_orc_events(&raw);
        assert_eq!(events.len(), 1);
        match &events[0] {
            OrcEvent::Result {
                is_error,
                result,
                cost_usd,
                duration_ms,
            } => {
                assert!(!is_error);
                assert_eq!(result, "done");
                assert_eq!(*cost_usd, Some(0.05));
                assert_eq!(*duration_ms, Some(5000));
            }
            other => panic!("expected Result, got {:?}", other),
        }
    }

    #[test]
    fn parse_result_error() {
        let raw = serde_json::json!({
            "type": "result",
            "subtype": "error",
            "result": "something failed"
        });
        let events = parse_orc_events(&raw);
        assert_eq!(events.len(), 1);
        match &events[0] {
            OrcEvent::Result { is_error, .. } => assert!(is_error),
            other => panic!("expected Result, got {:?}", other),
        }
    }

    #[test]
    fn orc_usage_default() {
        let usage = OrcUsage::default();
        assert_eq!(usage.total_input, 0);
        assert_eq!(usage.total_output, 0);
        assert_eq!(usage.total_cost, 0.0);
        assert_eq!(usage.turns, 0);
    }

    #[test]
    fn parse_unknown_type_returns_empty() {
        let raw = serde_json::json!({"type": "unknown_type", "data": 42});
        let events = parse_orc_events(&raw);
        assert!(events.is_empty());
    }

    #[test]
    fn parse_empty_content_array() {
        let raw = serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": []}
        });
        let events = parse_orc_events(&raw);
        assert!(events.is_empty());
    }
}
