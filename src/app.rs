use crate::agent::{Agent, AgentState, OutputEntry};
use crate::claude::ClaudeProcess;
use crate::events::{ContentBlock, StreamEvent};
use std::time::Instant;

/// Dashboard interaction modes
pub enum AppMode {
    Dashboard,
    Input {
        prompt_label: String,
        callback: InputCallback,
    },
    AgentDetail {
        agent_idx: usize,
        scroll: usize,
        browsing: bool,
    },
    Help,
    Confirm {
        message: String,
        callback: ConfirmCallback,
    },
}

pub enum InputCallback {
    ChatOrc,
    TellAgent,
    NewAgent,
    DirectSend,
}

pub enum ConfirmCallback {
    KillAgent(usize),
}

/// Commands the orc brain can embed in its responses.
pub enum OrcCommand {
    Spawn { name: String, task: String },
    Tell { name: String, message: String },
    Kill { name: String },
}

/// Strip all command blocks ([SPAWN_AGENT ...], [TELL_AGENT ...], [KILL_AGENT ...]) from text.
/// Returns the text with command blocks removed, suitable for display.
pub fn strip_orc_commands(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;

    while !remaining.is_empty() {
        let next_cmd = ["[SPAWN_AGENT", "[TELL_AGENT", "[KILL_AGENT"]
            .iter()
            .filter_map(|tag| remaining.find(tag))
            .min();

        match next_cmd {
            Some(start) => {
                // Keep text before the command
                result.push_str(&remaining[..start]);
                let cmd_start = &remaining[start..];
                // Find end of command block
                let end = cmd_start.find("\"]")
                    .map(|p| p + 2)
                    .or_else(|| cmd_start.find(']').map(|p| p + 1))
                    .unwrap_or(cmd_start.len());
                remaining = &remaining[start + end..];
            }
            None => {
                result.push_str(remaining);
                break;
            }
        }
    }

    // Clean up: remove empty lines left by stripping
    result.lines()
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse [SPAWN_AGENT ...], [TELL_AGENT ...], [KILL_AGENT ...] from orc output.
/// Handles multi-line commands — finds opening `[CMD` and scans until closing `"]`.
pub fn parse_orc_commands(text: &str) -> Vec<OrcCommand> {
    let mut cmds = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        // Find the next command start
        let (cmd_type, start) = [
            ("[SPAWN_AGENT", "spawn"),
            ("[TELL_AGENT", "tell"),
            ("[KILL_AGENT", "kill"),
        ]
        .iter()
        .filter_map(|(tag, kind)| remaining.find(tag).map(|pos| (*kind, pos)))
        .min_by_key(|(_, pos)| *pos)
        .unwrap_or(("", remaining.len()));

        if start >= remaining.len() {
            break;
        }

        let cmd_start = &remaining[start..];
        // Find closing `]` — the command ends at the first `"]` or `]`
        let end = cmd_start.find("\"]")
            .map(|p| p + 2)
            .or_else(|| cmd_start.find(']').map(|p| p + 1));

        let block = match end {
            Some(e) => &cmd_start[..e],
            None => {
                remaining = &remaining[start + 1..];
                continue;
            }
        };

        match cmd_type {
            "spawn" => {
                if let (Some(name), Some(task)) = (
                    extract_attr(block, "name"),
                    extract_attr_multiline(block, "task"),
                ) {
                    cmds.push(OrcCommand::Spawn { name, task });
                }
            }
            "tell" => {
                if let (Some(name), Some(message)) = (
                    extract_attr(block, "name"),
                    extract_attr_multiline(block, "message"),
                ) {
                    cmds.push(OrcCommand::Tell { name, message });
                }
            }
            "kill" => {
                if let Some(name) = extract_attr(block, "name") {
                    cmds.push(OrcCommand::Kill { name });
                }
            }
            _ => {}
        }

        remaining = &remaining[start + end.unwrap_or(1)..];
    }

    cmds
}

/// Extract a short attribute (single line, no embedded quotes).
fn extract_attr(s: &str, attr: &str) -> Option<String> {
    let pattern = format!("{}=\"", attr);
    let start = s.find(&pattern)? + pattern.len();
    let rest = &s[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Extract an attribute whose value may span multiple lines.
/// Finds `attr="` and reads until the closing `"` before `]`.
fn extract_attr_multiline(s: &str, attr: &str) -> Option<String> {
    let pattern = format!("{}=\"", attr);
    let start = s.find(&pattern)? + pattern.len();
    let rest = &s[start..];
    // Find the last `"` before the end (handles embedded content)
    let end = rest.rfind('"')?;
    if end == 0 {
        return None;
    }
    Some(rest[..end].to_string())
}

pub struct App {
    pub agents: Vec<Agent>,
    pub selected: usize,
    pub mode: AppMode,
    pub input_buf: String,
    pub should_quit: bool,
    pub project_dir: String,
    pub last_poll: Instant,
    pub status_line: String,
    pub status_line_at: Instant,
    pub orc: Option<ClaudeProcess>,
    pub orc_output: Vec<OutputEntry>,
    pub orc_pending_cmds: Vec<OrcCommand>,
    pub orc_session_id: Option<String>,
    pub show_preview: bool,
    pub scroll_offset: usize,
    pub agent_input_buf: String,
    pub agent_input_name: String,
}

impl App {
    pub fn new(project_dir: &str) -> Self {
        Self {
            agents: Vec::new(),
            selected: 0,
            mode: AppMode::Input {
                prompt_label: "> ".to_string(),
                callback: InputCallback::ChatOrc,
            },
            input_buf: String::new(),
            should_quit: false,
            project_dir: project_dir.to_string(),
            last_poll: Instant::now(),
            status_line: String::new(),
            status_line_at: Instant::now(),
            orc: None,
            orc_output: Vec::new(),
            orc_pending_cmds: Vec::new(),
            orc_session_id: None,
            show_preview: true,
            scroll_offset: 0,
            agent_input_buf: String::new(),
            agent_input_name: String::new(),
        }
    }

    pub fn set_status(&mut self, msg: String) {
        self.status_line = msg;
        self.status_line_at = Instant::now();
    }

    pub fn status_display(&self) -> String {
        if self.status_line.is_empty() {
            return String::new();
        }
        let ago = self.status_line_at.elapsed().as_secs();
        let time = if ago < 5 {
            "just now".to_string()
        } else if ago < 60 {
            format!("{}s ago", ago)
        } else if ago < 3600 {
            format!("{}m ago", ago / 60)
        } else {
            format!("{}h ago", ago / 3600)
        };
        format!("{} ({})", self.status_line, time)
    }

    pub fn selected_agent(&self) -> Option<&Agent> {
        self.agents.get(self.selected)
    }

    pub fn select_next(&mut self) {
        if !self.agents.is_empty() {
            self.selected = (self.selected + 1) % self.agents.len();
            self.scroll_offset = 0;
        }
    }

    pub fn select_prev(&mut self) {
        if !self.agents.is_empty() {
            self.selected = if self.selected == 0 {
                self.agents.len() - 1
            } else {
                self.selected - 1
            };
            self.scroll_offset = 0;
        }
    }

    /// Drain available events from the orc process and all agent processes.
    /// Non-blocking: reads only what's available, doesn't wait.
    /// Returns a list of (agent_name, result_text, is_error) for agents that just completed.
    pub fn drain_events(&mut self) -> Vec<(String, String, bool)> {
        self.drain_orc_events();

        let mut completions = Vec::new();
        for agent in &mut self.agents {
            let was_done = matches!(agent.state, AgentState::Done | AgentState::Error);
            drain_agent_events(agent);
            let is_done = matches!(agent.state, AgentState::Done | AgentState::Error);

            if !was_done && is_done {
                let result_text = agent.output.recent(1)
                    .first()
                    .map(|e| match e {
                        OutputEntry::Result { text, .. } => text.clone(),
                        OutputEntry::Text(t) => t.clone(),
                        _ => String::new(),
                    })
                    .unwrap_or_default();
                let is_error = matches!(agent.state, AgentState::Error);
                completions.push((agent.name.clone(), result_text, is_error));
            }
        }

        self.last_poll = Instant::now();
        completions
    }

    fn drain_orc_events(&mut self) {
        let orc = match self.orc.as_mut() {
            Some(o) => o,
            None => return,
        };

        loop {
            match orc.try_read_event() {
                Ok(Some(event)) => {
                    match &event {
                        StreamEvent::System { subtype, session_id, .. } => {
                            if subtype == "init" {
                                if let Some(sid) = session_id {
                                    self.orc_session_id = Some(sid.clone());
                                }
                            }
                        }
                        StreamEvent::Assistant { message, .. } => {
                            for block in &message.content {
                                match block {
                                    ContentBlock::Text { text } => {
                                        // Parse commands from raw text before filtering
                                        let mut cmds = parse_orc_commands(text);
                                        self.orc_pending_cmds.append(&mut cmds);

                                        // Strip command blocks from display text
                                        let filtered = strip_orc_commands(text);
                                        if !filtered.trim().is_empty() {
                                            self.orc_output.push(OutputEntry::Text(filtered));
                                        }
                                    }
                                    ContentBlock::ToolUse { name, input, .. } => {
                                        let input_str = input.to_string();
                                        self.orc_output.push(OutputEntry::ToolUse {
                                            name: name.clone(),
                                            input: input_str,
                                        });
                                    }
                                    _ => {}
                                }
                            }
                        }
                        StreamEvent::Result { is_error, .. } => {
                            // Don't push result text — it duplicates the assistant text
                            // already streamed. Only track errors.
                            if *is_error {
                                if let StreamEvent::Result { result: Some(text), .. } = &event {
                                    self.orc_output.push(OutputEntry::Result {
                                        text: text.clone(),
                                        is_error: true,
                                    });
                                }
                            }
                        }
                        StreamEvent::Other => {}
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
    }
}

fn drain_agent_events(agent: &mut Agent) {
    let proc = match agent.process.as_mut() {
        Some(p) => p,
        None => return,
    };

    loop {
        match proc.try_read_event() {
            Ok(Some(event)) => {
                match &event {
                    StreamEvent::System { subtype, session_id, .. } => {
                        if subtype == "init" {
                            if let Some(sid) = session_id {
                                agent.session_id = Some(sid.clone());
                            }
                        }
                    }
                    StreamEvent::Assistant { message, .. } => {
                        agent.state = AgentState::Working;
                        for block in &message.content {
                            match block {
                                ContentBlock::Text { text } => {
                                    agent.output.push_text(text);
                                }
                                ContentBlock::ToolUse { name, input, .. } => {
                                    agent.output.push_tool_use(name, &input.to_string());
                                }
                                _ => {}
                            }
                        }
                    }
                    StreamEvent::Result { result, is_error, total_cost_usd, .. } => {
                        if let Some(cost) = total_cost_usd {
                            agent.cost_usd += cost;
                        }
                        if *is_error {
                            agent.state = AgentState::Error;
                            if let Some(text) = result {
                                agent.output.push_result(text, true);
                            }
                        } else {
                            agent.state = AgentState::Done;
                            if let Some(text) = result {
                                agent.output.push_result(text, false);
                            }
                        }
                    }
                    StreamEvent::Other => {}
                }
            }
            Ok(None) | Err(_) => break,
        }
    }

    // Check if process died unexpectedly
    if !proc.is_alive() && agent.state == AgentState::Working {
        agent.state = AgentState::Error;
        agent.output.push_result("process exited unexpectedly", true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_app() -> App {
        App::new("/tmp")
    }

    fn make_agent(name: &str) -> Agent {
        Agent::new_without_process(
            name.to_string(),
            "task".to_string(),
            PathBuf::from("/tmp"),
        )
    }

    #[test]
    fn test_select_next_wraps() {
        let mut app = make_app();
        app.agents.push(make_agent("a"));
        app.agents.push(make_agent("b"));
        app.selected = 1;
        app.select_next();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn test_select_prev_wraps() {
        let mut app = make_app();
        app.agents.push(make_agent("a"));
        app.agents.push(make_agent("b"));
        app.selected = 0;
        app.select_prev();
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn test_select_next_empty() {
        let mut app = make_app();
        app.select_next();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn test_select_resets_scroll() {
        let mut app = make_app();
        app.agents.push(make_agent("a"));
        app.agents.push(make_agent("b"));
        app.scroll_offset = 10;
        app.select_next();
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn test_set_status() {
        let mut app = make_app();
        app.set_status("hello".to_string());
        assert!(app.status_display().contains("hello"));
    }

    #[test]
    fn test_status_display_empty() {
        let app = make_app();
        assert_eq!(app.status_display(), "");
    }

    #[test]
    fn test_selected_agent_none_when_empty() {
        let app = make_app();
        assert!(app.selected_agent().is_none());
    }

    #[test]
    fn test_selected_agent_some() {
        let mut app = make_app();
        app.agents.push(make_agent("test"));
        assert_eq!(app.selected_agent().unwrap().name, "test");
    }

    #[test]
    fn test_parse_orc_commands_spawn() {
        let text = r#"I'll spawn an agent for this.
[SPAWN_AGENT name="fix-bug" task="Fix the login bug in src/auth.rs"]
"#;
        let cmds = parse_orc_commands(text);
        assert_eq!(cmds.len(), 1);
        match &cmds[0] {
            OrcCommand::Spawn { name, task } => {
                assert_eq!(name, "fix-bug");
                assert_eq!(task, "Fix the login bug in src/auth.rs");
            }
            _ => panic!("expected Spawn"),
        }
    }

    #[test]
    fn test_parse_orc_commands_tell() {
        let text = r#"[TELL_AGENT name="fix-bug" message="Also check the tests"]"#;
        let cmds = parse_orc_commands(text);
        assert_eq!(cmds.len(), 1);
        match &cmds[0] {
            OrcCommand::Tell { name, message } => {
                assert_eq!(name, "fix-bug");
                assert_eq!(message, "Also check the tests");
            }
            _ => panic!("expected Tell"),
        }
    }

    #[test]
    fn test_parse_orc_commands_kill() {
        let text = r#"[KILL_AGENT name="old-agent"]"#;
        let cmds = parse_orc_commands(text);
        assert_eq!(cmds.len(), 1);
        assert!(matches!(cmds[0], OrcCommand::Kill { .. }));
    }

    #[test]
    fn test_parse_orc_commands_multiple() {
        let text = r#"Let me spawn two agents:
[SPAWN_AGENT name="frontend" task="Build the UI"]
[SPAWN_AGENT name="backend" task="Build the API"]
"#;
        let cmds = parse_orc_commands(text);
        assert_eq!(cmds.len(), 2);
    }

    #[test]
    fn test_parse_orc_commands_none() {
        let text = "Just some regular text, no commands here.";
        let cmds = parse_orc_commands(text);
        assert_eq!(cmds.len(), 0);
    }

    #[test]
    fn test_strip_orc_commands_single_line() {
        let text = r#"I'll spawn an agent.
[SPAWN_AGENT name="fix" task="Fix bug"]
Done."#;
        let stripped = strip_orc_commands(text);
        assert!(stripped.contains("I'll spawn an agent."));
        assert!(stripped.contains("Done."));
        assert!(!stripped.contains("SPAWN_AGENT"));
    }

    #[test]
    fn test_strip_orc_commands_multiline() {
        let text = r#"Here's the plan:
[SPAWN_AGENT name="dark-mode" task="Add dark mode.
Steps:
1. Read theme
2. Add toggle"]
Working on it."#;
        let stripped = strip_orc_commands(text);
        assert!(stripped.contains("Here's the plan:"));
        assert!(stripped.contains("Working on it."));
        assert!(!stripped.contains("SPAWN_AGENT"));
        assert!(!stripped.contains("dark-mode"));
    }

    #[test]
    fn test_strip_orc_commands_no_commands() {
        let text = "Just regular text, nothing special.";
        let stripped = strip_orc_commands(text);
        assert_eq!(stripped, text);
    }

    #[test]
    fn test_strip_orc_commands_only_command() {
        let text = r#"[SPAWN_AGENT name="x" task="y"]"#;
        let stripped = strip_orc_commands(text);
        assert!(stripped.trim().is_empty());
    }

    #[test]
    fn test_parse_orc_commands_multiline() {
        let text = r#"Here's the plan:
[SPAWN_AGENT name="dark-mode" task="Add dark mode toggle.
Steps:
1. Read theme files
2. Add toggle button
3. Persist preference"]
Done."#;
        let cmds = parse_orc_commands(text);
        assert_eq!(cmds.len(), 1);
        match &cmds[0] {
            OrcCommand::Spawn { name, task } => {
                assert_eq!(name, "dark-mode");
                assert!(task.contains("Steps:"));
                assert!(task.contains("Persist preference"));
            }
            _ => panic!("expected Spawn"),
        }
    }
}
