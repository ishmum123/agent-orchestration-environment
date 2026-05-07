use crate::events::StreamEvent;
use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

/// Format a user message as NDJSON for stdin.
pub fn format_user_message(content: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": content
        }
    })
    .to_string()
}

/// Parse a single NDJSON line into a StreamEvent.
pub fn parse_event_line(line: &str) -> Result<StreamEvent> {
    serde_json::from_str(line).context("failed to parse stream event")
}

/// Builder for `claude -p` CLI arguments.
pub struct ClaudeArgs {
    system_prompt: Option<String>,
    model: Option<String>,
    resume_session: Option<String>,
    tools: Option<String>,
    permission_mode: Option<String>,
    add_dir: Option<String>,
    extra_args: Vec<String>,
}

impl ClaudeArgs {
    pub fn new() -> Self {
        Self {
            system_prompt: None,
            model: None,
            resume_session: None,
            tools: None,
            permission_mode: None,
            add_dir: None,
            extra_args: Vec::new(),
        }
    }

    pub fn system_prompt(mut self, prompt: &str) -> Self {
        self.system_prompt = Some(prompt.to_string());
        self
    }

    pub fn model(mut self, model: &str) -> Self {
        self.model = Some(model.to_string());
        self
    }

    pub fn resume(mut self, session_id: &str) -> Self {
        self.resume_session = Some(session_id.to_string());
        self
    }

    pub fn tools(mut self, tools: &str) -> Self {
        self.tools = Some(tools.to_string());
        self
    }

    pub fn permission_mode(mut self, mode: &str) -> Self {
        self.permission_mode = Some(mode.to_string());
        self
    }

    pub fn add_dir(mut self, dir: &str) -> Self {
        self.add_dir = Some(dir.to_string());
        self
    }

    pub fn build(self) -> Vec<String> {
        let mut args = vec![
            "-p".to_string(),
            "--input-format".to_string(),
            "stream-json".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
        ];

        if let Some(ref prompt) = self.system_prompt {
            args.push("--system-prompt".to_string());
            args.push(prompt.clone());
        }
        if let Some(ref model) = self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(ref session_id) = self.resume_session {
            args.push("--resume".to_string());
            args.push(session_id.clone());
        }
        if let Some(ref tools) = self.tools {
            args.push("--tools".to_string());
            args.push(tools.clone());
        }
        if let Some(ref mode) = self.permission_mode {
            args.push("--permission-mode".to_string());
            args.push(mode.clone());
        }
        if let Some(ref dir) = self.add_dir {
            args.push("--add-dir".to_string());
            args.push(dir.clone());
        }
        args.extend(self.extra_args);
        args
    }
}

/// A running Claude Code child process with piped stdin/stdout.
pub struct ClaudeProcess {
    pub child: Child,
    pub session_id: Option<String>,
    reader: BufReader<std::process::ChildStdout>,
}

impl ClaudeProcess {
    /// Spawn a new `claude` child process with the given args.
    pub fn spawn(args: ClaudeArgs, working_dir: &str) -> Result<Self> {
        let built_args = args.build();
        let mut child = Command::new("claude")
            .args(&built_args)
            .current_dir(working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to spawn claude process")?;

        let stdout = child.stdout.take().context("no stdout pipe")?;
        let reader = BufReader::new(stdout);

        Ok(Self {
            child,
            session_id: None,
            reader,
        })
    }

    /// Send a user message to the process via stdin.
    pub fn send(&mut self, content: &str) -> Result<()> {
        let stdin = self.child.stdin.as_mut().context("no stdin pipe")?;
        let msg = format_user_message(content);
        writeln!(stdin, "{}", msg).context("failed to write to claude stdin")?;
        stdin.flush().context("failed to flush claude stdin")
    }

    /// Try to read and parse the next NDJSON line from stdout.
    /// Returns `None` if the process has exited (EOF).
    pub fn try_read_event(&mut self) -> Result<Option<StreamEvent>> {
        let mut line = String::new();
        let bytes = self.reader.read_line(&mut line)
            .context("failed to read from claude stdout")?;
        if bytes == 0 {
            return Ok(None); // EOF
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let event = parse_event_line(trimmed)?;

        // Capture session_id from init event
        if let StreamEvent::System { subtype, session_id: Some(ref sid), .. } = &event {
            if subtype == "init" {
                self.session_id = Some(sid.clone());
            }
        }

        Ok(Some(event))
    }

    /// Check if the child process is still alive.
    pub fn is_alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    /// Kill the child process.
    pub fn kill(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

impl Drop for ClaudeProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_user_message() {
        let msg = format_user_message("hello world");
        let parsed: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(parsed["type"], "user");
        assert_eq!(parsed["message"]["role"], "user");
        assert_eq!(parsed["message"]["content"], "hello world");
    }

    #[test]
    fn test_format_user_message_special_chars() {
        let msg = format_user_message("say \"hello\" & 'goodbye'");
        let parsed: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(parsed["message"]["content"], "say \"hello\" & 'goodbye'");
    }

    #[test]
    fn test_parse_event_line() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"s1","total_cost_usd":0.01}"#;
        let event = parse_event_line(line).unwrap();
        assert!(matches!(event, StreamEvent::Result { .. }));
    }

    #[test]
    fn test_parse_event_line_invalid_json() {
        let result = parse_event_line("not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_claude_args_basic() {
        let args = ClaudeArgs::new()
            .system_prompt("you are helpful")
            .model("haiku")
            .build();
        assert!(args.contains(&"--system-prompt".to_string()));
        assert!(args.contains(&"you are helpful".to_string()));
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"haiku".to_string()));
        assert!(args.contains(&"--input-format".to_string()));
        assert!(args.contains(&"stream-json".to_string()));
    }

    #[test]
    fn test_claude_args_with_resume() {
        let args = ClaudeArgs::new()
            .resume("abc-123")
            .build();
        assert!(args.contains(&"--resume".to_string()));
        assert!(args.contains(&"abc-123".to_string()));
    }

    #[test]
    fn test_claude_args_with_tools_disabled() {
        let args = ClaudeArgs::new()
            .tools("")
            .build();
        assert!(args.contains(&"--tools".to_string()));
        assert!(args.contains(&"".to_string()));
    }

    #[test]
    fn test_claude_args_with_permission_mode() {
        let args = ClaudeArgs::new()
            .permission_mode("auto")
            .build();
        assert!(args.contains(&"--permission-mode".to_string()));
        assert!(args.contains(&"auto".to_string()));
    }

    #[test]
    fn test_claude_args_with_add_dir() {
        let args = ClaudeArgs::new()
            .add_dir("/some/path")
            .build();
        assert!(args.contains(&"--add-dir".to_string()));
        assert!(args.contains(&"/some/path".to_string()));
    }
}
