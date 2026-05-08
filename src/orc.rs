// Orchestrator brain: spawn and manage the planning agent.

use anyhow::Result;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

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

    /// Try to read the next event from the orc brain.
    /// Returns None if no data is available yet or on timeout.
    pub async fn try_read_event(&mut self) -> Result<Option<serde_json::Value>> {
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

/// Generate MCP config file for the orc brain.
/// Returns the path to the generated config file.
pub async fn write_mcp_config(orc_binary: &Path) -> Result<PathBuf> {
    let config = crate::mcp::generate_mcp_config(orc_binary.to_str().unwrap_or("orc"));
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
        "--permission-mode",
        "plan",
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
    async fn mcp_config_written() {
        let binary = Path::new("/usr/local/bin/orc");
        let path = write_mcp_config(binary).await.unwrap();
        assert!(path.exists());
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed["mcpServers"]["orc"].is_object());
        tokio::fs::remove_file(&path).await.ok();
    }
}
