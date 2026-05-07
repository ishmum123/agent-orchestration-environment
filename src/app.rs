use crate::agent::{Agent, AgentState, OutputEntry};
use crate::claude::ClaudeProcess;
use crate::events::{ContentBlock, StreamEvent};
use std::path::PathBuf;
use std::time::Instant;

/// Dashboard interaction modes
pub enum AppMode {
    Dashboard,
    Input {
        prompt_label: String,
        callback: InputCallback,
    },
    Status,
    AgentDetail {
        agent_idx: usize,
        scroll: usize,
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

/// Parse [SPAWN_AGENT ...], [TELL_AGENT ...], [KILL_AGENT ...] from orc output.
pub fn parse_orc_commands(text: &str) -> Vec<OrcCommand> {
    let mut cmds = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("[SPAWN_AGENT") {
            if let (Some(name), Some(task)) = (
                extract_attr(trimmed, "name"),
                extract_attr(trimmed, "task"),
            ) {
                cmds.push(OrcCommand::Spawn { name, task });
            }
        } else if trimmed.starts_with("[TELL_AGENT") {
            if let (Some(name), Some(message)) = (
                extract_attr(trimmed, "name"),
                extract_attr(trimmed, "message"),
            ) {
                cmds.push(OrcCommand::Tell { name, message });
            }
        } else if trimmed.starts_with("[KILL_AGENT") {
            if let Some(name) = extract_attr(trimmed, "name") {
                cmds.push(OrcCommand::Kill { name });
            }
        }
    }

    cmds
}

/// Extract a named attribute value from a string like `name="value"`.
fn extract_attr(s: &str, attr: &str) -> Option<String> {
    let pattern = format!("{}=\"", attr);
    let start = s.find(&pattern)? + pattern.len();
    let rest = &s[start..];
    let end = rest.find('"')?;
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
    pub orc_cmds_processed: usize,
    pub orc_session_id: Option<String>,
    pub show_preview: bool,
    pub scroll_offset: usize,
    pub status_selected: usize,
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
            orc_cmds_processed: 0,
            orc_session_id: None,
            show_preview: true,
            scroll_offset: 0,
            status_selected: 0,
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
    pub fn drain_events(&mut self) {
        self.drain_orc_events();

        for agent in &mut self.agents {
            drain_agent_events(agent);
        }

        self.last_poll = Instant::now();
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
                                        self.orc_output.push(OutputEntry::Text(text.clone()));
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
}
