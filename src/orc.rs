// Orchestrator brain: spawn and manage the planning agent.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::worker::{parse_rate_limit_event, RateLimitStatus};

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
    /// Rate-limit state change (warning or exceeded).
    RateLimit {
        status: RateLimitStatus,
        resets_at: DateTime<Utc>,
        bucket: String,
    },
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
        "rate_limit_event" => {
            if let Some((status, resets_at, bucket)) = parse_rate_limit_event(raw) {
                events.push(OrcEvent::RateLimit {
                    status,
                    resets_at,
                    bucket,
                });
            }
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
    /// Latest assistant turn's full context-window occupancy in tokens
    /// (input + cache_read + cache_creation). 0 until first turn lands.
    pub last_context_tokens: u64,
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
        self.send_with(message, &[]).await
    }

    /// Send variant that attaches image content blocks. Empty
    /// `attachments` produces the original text-content shape.
    pub async fn send_with(
        &mut self,
        message: &str,
        attachments: &[crate::input_attachments::Attachment],
    ) -> Result<()> {
        let msg = crate::input_attachments::build_user_message(message, attachments);
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
                // Track token counts from assistant messages.
                // input/output accumulate; last_context_tokens reflects
                // the latest turn's full window occupancy (incl. cache).
                if let Some(usage) = raw.pointer("/message/usage") {
                    let input = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let output = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let cache_read = usage
                        .get("cache_read_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let cache_creation = usage
                        .get("cache_creation_input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    self.usage.total_input += input;
                    self.usage.total_output += output;
                    let total = input + cache_read + cache_creation;
                    if total > 0 {
                        self.usage.last_context_tokens = total;
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

    /// Cancel the orc brain's in-flight turn (SIGINT). The conversation stays
    /// alive — claude treats SIGINT as cancel-current-turn.
    pub async fn interrupt(&mut self) -> Result<()> {
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGINT);
            }
        }
        Ok(())
    }

    /// Inject a synthetic user message describing a worker's pending question.
    /// Orc may answer via the `answer_worker` MCP tool.
    pub async fn inject_question(
        &mut self,
        worker_session_id: &str,
        question_id: &str,
        question: &str,
    ) -> Result<()> {
        let content = format!(
            "[worker {worker_session_id} asked, question_id={question_id}]: {question}\n\
             You may answer it by calling the `answer_worker` MCP tool with the question_id and your answer. \
             If you don't have enough context, stay quiet — the user will be prompted."
        );
        self.send(&content).await
    }
}

/// Generate the system prompt for the orc brain.
pub fn system_prompt(project_dir: &Path) -> String {
    format!(
        r#"You are **orc**, an orchestration agent. You manage worker sessions to accomplish the user's goals — you do NOT investigate, read code, or write code yourself. You coordinate workers that do.

## Forward Motion (highest priority)

Default to action. The user opted into orc so they can leave and come back to finished work, not to a stack of tabs waiting on them.

- **Never ask in chat.** All user-facing questions go through `mcp__orc__ask_user`. Chat is for status only. If you're about to write a sentence ending in `?`, stop and call `ask_user`.
- **Never investigate or answer substantively yourself.** Anything involving file reads, commands, web research, or output longer than ~3 lines MUST be delegated. Your turn is at most: a brief status line, a `current_summary` call, and a `spawn_session` / `instruct_session`.
- **Keep `ask_user` brief** — 1–2 sentence question. The user already sees your reasoning in chat.
- **When in doubt, spawn.** Spawning is cheap; deliberating is expensive.

## Tools

- `spawn_session(name, task, model?)` — new worker with its own git worktree. Default `sonnet`; `opus` for hard reasoning, `haiku` for trivial lookups.
- `respawn_session(stale_session_id, handoff)` — kill `stale_session_id` and start a fresh worker in its worktree. `handoff` carries the reviewer's last verdict so the new worker resumes with context.
- `instruct_session(session_id, message)` — send instructions.
- `kill_session(session_id)` — kill and clean up.
- `list_sessions()` — check status. Call before deciding anything.
- `submit_for_review(session_id, summary)` — mark a worker's diff ready for review. Workers usually call this themselves.
- `merge_session(session_id, target_branch, push)` — merge a finished worker into `target_branch`, optionally pushing. Only call when the user has pre-authorized.
- `mark_done(session_id, summary)` — close out without merging.
- `ask_user(question, context?)` — blocks until reply. Use sparingly.
- `answer_worker(session_id, answer)` — race to answer a worker's question if you have context; stay quiet if not.
- `current_summary(summary)` — one-sentence "what I'm doing now" for the agents panel. Call after meaningful progress, not every line.
- `update_task_graph(graph)` — update the plan.

## Workflow

1. Read the user's task. **At the task boundary, never mid-flight**, capture: should results merge? On what branch? Pushed?
   - Explicit in the request ("fix and merge to main, push") → record it.
   - Ambiguous and merge could be in scope → `ask_user` ONCE up front. Never ask about merge again.
2. `update_task_graph` with the plan.
3. `spawn_session` for each subtask. Parallel where independent. Max 5 concurrent.
4. When a worker calls `submit_for_review`, **spawn a reviewer worker** (`spawn_session`, fresh context, with the original worker's worktree path in its task body so it can read the diff via absolute paths). Its verdict (returned via `mark_done` summary) is the gate.
5. On **pass**: pre-authorized → `merge_session`. Otherwise → `mark_done`.
6. On **fail**: `instruct_session` the original worker with the reviewer's feedback. Re-review when it resubmits.
7. **3-strikes respawn**: if the same worker fails review 3 times, `respawn_session` once with the reviewer's last verdict as handoff. If the fresh worker also fails 3 times, only THEN call `ask_user` — explain and ask how to proceed.
8. When all subtasks are done, summarize results in chat.

## Silent internal-status messages

The harness will inject messages framed as
`[internal status — do NOT reply or narrate to the user; the UI has already notified them]`.

- **Ingest internally.** Treat as new context.
- **Write zero assistant text.** Not an acknowledgement, not a recap. The user already sees the same fact in the UI; echoing duplicates it on their screen.
- **Only act** if it changes what you must do. The action (a tool call) is the response — still no chat.
- A turn ending with zero assistant text after an internal-status is correct.

## Conventions

- Name sessions descriptively (`auth-fix`, `add-tests`, `refactor-api`).
- Keep responses concise.

## Project

Working directory: {project_dir}
"#,
        project_dir = project_dir.display()
    )
}

/// Generate MCP config file for the orc brain (HTTP transport).
/// The MCP server must already be running on the given port.
/// Returns the path to the generated config file. The caller passes
/// the per-project `.orc/` directory so config lives with the project.
pub async fn write_mcp_config(data_dir: &Path, port: u16) -> Result<PathBuf> {
    let config = crate::mcp::generate_mcp_config(port);
    tokio::fs::create_dir_all(data_dir).await.ok();
    let config_path = data_dir.join("mcp-config.json");
    tokio::fs::write(&config_path, serde_json::to_string_pretty(&config)?).await?;
    Ok(config_path)
}

/// Spawn the orc brain as a Claude Code process. Pass `resume_session_id`
/// to attach via `--resume <id>` and preserve conversation continuity
/// (used by the quota scheduler after a paused respawn).
pub async fn spawn_orc(config: &OrcConfig) -> Result<OrcProcess> {
    spawn_orc_inner(config, None).await
}

pub async fn spawn_orc_resume(
    config: &OrcConfig,
    claude_session_id: &str,
) -> Result<OrcProcess> {
    spawn_orc_inner(config, Some(claude_session_id)).await
}

async fn spawn_orc_inner(
    config: &OrcConfig,
    resume_session_id: Option<&str>,
) -> Result<OrcProcess> {
    let prompt = system_prompt(&config.project_dir);

    let mut cmd = Command::new(crate::worker::claude_bin());
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
    if let Some(sid) = resume_session_id {
        cmd.args(["--resume", sid]);
    }
    cmd.env("CLAUDE_AUTOCOMPACT_PCT_OVERRIDE", "50");
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
        assert!(prompt.contains("respawn_session"));
        assert!(prompt.contains("instruct_session"));
        assert!(prompt.contains("kill_session"));
        assert!(prompt.contains("ask_user"));
        assert!(prompt.contains("list_sessions"));
        assert!(prompt.contains("mark_done"));
        assert!(prompt.contains("merge_session"));
        assert!(prompt.contains("update_task_graph"));
        assert!(prompt.contains("/tmp/test-project"));
    }

    #[test]
    fn system_prompt_has_rules() {
        let prompt = system_prompt(Path::new("/tmp/test"));
        assert!(prompt.contains("Never ask in chat"));
        assert!(prompt.contains("Never investigate"));
        assert!(prompt.contains("Max 5 concurrent"));
        assert!(prompt.contains("3-strikes respawn"));
        assert!(prompt.contains("reviewer worker"));
    }

    #[tokio::test]
    async fn mcp_config_written_http() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_mcp_config(tmp.path(), 9999).await.unwrap();
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
