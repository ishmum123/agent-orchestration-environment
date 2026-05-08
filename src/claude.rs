use crate::events::StreamEvent;
use anyhow::{Context, Result};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
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
    tools: Option<String>,
    disallowed_tools: Option<String>,
    permission_mode: Option<String>,
    no_mcp: bool,
    stderr_log: Option<PathBuf>,
    extra_args: Vec<String>,
}

impl ClaudeArgs {
    pub fn new() -> Self {
        Self {
            system_prompt: None,
            model: None,
            tools: None,
            disallowed_tools: None,
            permission_mode: None,
            no_mcp: false,
            stderr_log: None,
            extra_args: Vec::new(),
        }
    }

    /// Disable all MCP servers (uses --strict-mcp-config with no config).
    pub fn no_mcp(mut self) -> Self {
        self.no_mcp = true;
        self
    }

    pub fn stderr_log(mut self, path: PathBuf) -> Self {
        self.stderr_log = Some(path);
        self
    }

    pub fn disallowed_tools(mut self, tools: &str) -> Self {
        self.disallowed_tools = Some(tools.to_string());
        self
    }

    pub fn system_prompt(mut self, prompt: &str) -> Self {
        self.system_prompt = Some(prompt.to_string());
        self
    }

    pub fn model(mut self, model: &str) -> Self {
        self.model = Some(model.to_string());
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
        if let Some(ref tools) = self.tools {
            args.push("--tools".to_string());
            args.push(tools.clone());
        }
        if let Some(ref tools) = self.disallowed_tools {
            args.push("--disallowed-tools".to_string());
            args.push(tools.clone());
        }
        if let Some(ref mode) = self.permission_mode {
            args.push("--permission-mode".to_string());
            args.push(mode.clone());
        }
        if self.no_mcp {
            args.push("--strict-mcp-config".to_string());
        }
        args.extend(self.extra_args);
        args
    }
}

/// A running Claude Code child process with piped stdin/stdout.
pub struct ClaudeProcess {
    pub child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl ClaudeProcess {
    /// Spawn a new `claude` child process with the given args.
    pub fn spawn(args: ClaudeArgs, working_dir: &str) -> Result<Self> {
        let stderr_log = args.stderr_log.clone();
        let built_args = args.build();
        let stderr_stdio = if let Some(ref log_path) = stderr_log {
            if let Some(parent) = log_path.parent() {
                fs::create_dir_all(parent).ok();
            }
            let file = fs::File::create(log_path)
                .context("failed to create stderr log file")?;
            Stdio::from(file)
        } else {
            Stdio::null()
        };
        let mut child = Command::new("claude")
            .args(&built_args)
            .current_dir(working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr_stdio)
            .spawn()
            .context("failed to spawn claude process")?;

        let stdout = child.stdout.take().context("no stdout pipe")?;

        // Set stdout to non-blocking so try_read_event doesn't hang the event loop
        use std::os::unix::io::AsRawFd;
        let fd = stdout.as_raw_fd();
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags == -1 {
                return Err(std::io::Error::last_os_error())
                    .context("fcntl F_GETFL failed on stdout pipe");
            }
            if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == -1 {
                return Err(std::io::Error::last_os_error())
                    .context("fcntl F_SETFL failed setting non-blocking");
            }
        }

        let reader = BufReader::new(stdout);

        Ok(Self {
            child,
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
    /// Returns `None` if no data is ready (WouldBlock) or EOF.
    pub fn try_read_event(&mut self) -> Result<Option<StreamEvent>> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => Ok(None), // EOF
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return Ok(None);
                }
                let event = parse_event_line(trimmed)?;
                Ok(Some(event))
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e.into()),
        }
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
    fn test_claude_args_with_disallowed_tools() {
        let args = ClaudeArgs::new()
            .disallowed_tools("LSP")
            .build();
        assert!(args.contains(&"--disallowed-tools".to_string()));
        assert!(args.contains(&"LSP".to_string()));
    }

    #[test]
    fn test_claude_args_no_mcp() {
        let args = ClaudeArgs::new()
            .no_mcp()
            .build();
        assert!(args.contains(&"--strict-mcp-config".to_string()));
    }

    #[test]
    fn test_set_nonblocking_does_not_panic() {
        use std::os::unix::io::AsRawFd;
        use std::process::{Command, Stdio};
        let mut child = Command::new("echo")
            .arg("test")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let fd = stdout.as_raw_fd();
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        child.wait().ok();
    }
}
