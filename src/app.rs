use crate::agent::{Agent, AgentMeta};
use crate::tmux::{self, TmuxPane, TmuxSession};
use anyhow::Result;
use std::path::PathBuf;
use std::time::Instant;

/// Dashboard interaction modes
pub enum AppMode {
    /// Main view — orc conversation + agent sidebar
    Dashboard,
    /// Typing an inline prompt
    Input {
        prompt_label: String,
        callback: InputCallback,
    },
    /// Viewing layered status summaries
    Status,
    /// Viewing full output of a single agent
    AgentDetail {
        agent_idx: usize,
        scroll: usize,
    },
    /// Help overlay showing all keybinds
    Help,
    /// Confirming a destructive action
    Confirm {
        message: String,
        callback: ConfirmCallback,
    },
}

/// What to do with inline input after Enter
pub enum InputCallback {
    ChatOrc,
    TellAgent,
    NewAgent,
    DirectSend,
}

/// What to do after confirming
pub enum ConfirmCallback {
    KillAgent(usize),
}

pub struct App {
    pub session: TmuxSession,
    pub agents: Vec<Agent>,
    pub selected: usize,
    pub mode: AppMode,
    pub input_buf: String,
    pub should_quit: bool,
    pub project_dir: String,
    pub last_poll: Instant,
    pub status_line: String,
    pub status_line_at: Instant,
    pub orc_pane: Option<TmuxPane>,
    pub orc_output: String,
    pub show_preview: bool,
    pub scroll_offset: usize,
    pub status_selected: usize,
    pub agent_input_buf: String,
    pub agent_input_name: String,
}

impl App {
    pub fn new(session_name: &str, project_dir: &str) -> Self {
        Self {
            session: TmuxSession::new(session_name),
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
            orc_pane: None,
            orc_output: String::new(),
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

    /// Poll orc pane and all agent panes for state updates
    pub fn poll_agents(&mut self) -> Result<()> {
        // Poll orc
        if let Some(orc_pane) = &self.orc_pane {
            if let Ok(output) = tmux::capture_pane(orc_pane, 100) {
                self.orc_output = output;
            }
        }

        // Discover orc-spawned agents
        self.discover_orc_agents();

        for agent in &mut self.agents {
            if agent.locked {
                continue;
            }
            if let Ok(output) = tmux::capture_pane(&agent.pane, 50) {
                agent.state = crate::agent::detect_state(&output);
                agent.last_output = output;
                agent.last_poll = Instant::now();
            }
        }
        self.last_poll = Instant::now();
        Ok(())
    }

    #[cfg(test)]
    pub fn discover_orc_agents_from(&mut self, agents_dir: &std::path::Path) {
        self._discover_orc_agents(Some(agents_dir));
    }

    /// Scan ~/.orc/agents/ for metadata files written by the orchestrator
    fn discover_orc_agents(&mut self) {
        self._discover_orc_agents(None);
    }

    fn _discover_orc_agents(&mut self, override_dir: Option<&std::path::Path>) {
        let default_dir = dirs::home_dir()
            .unwrap_or_default()
            .join(".orc/agents");
        let agents_dir = override_dir.unwrap_or(&default_dir);

        let entries = match std::fs::read_dir(agents_dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let contents = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let meta: AgentMeta = match serde_json::from_str(&contents) {
                Ok(m) => m,
                Err(_) => continue,
            };

            // Skip if we already track this agent
            if self.agents.iter().any(|a| a.name == meta.name) {
                continue;
            }

            let pane = TmuxPane {
                session: meta.session,
                window: meta.window,
                pane: meta.pane,
            };

            let agent = Agent::new(
                meta.name.clone(),
                meta.task,
                pane,
                PathBuf::from(meta.worktree),
            );
            self.agents.push(agent);
            self.set_status(format!("orc spawned '{}'", meta.name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_app() -> App {
        App::new("test-session", "/tmp")
    }

    fn make_agent(name: &str) -> Agent {
        Agent::new(
            name.to_string(),
            "task".to_string(),
            TmuxPane { session: "s".to_string(), window: 0, pane: 0 },
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
        app.select_next(); // should not panic
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
    fn test_discover_orc_agents() {
        let dir = TempDir::new().unwrap();
        let meta = r#"{
            "name": "discovered",
            "task": "do stuff",
            "session": "orc",
            "window": 0,
            "pane": 5,
            "worktree": "/tmp/wt"
        }"#;
        fs::write(dir.path().join("discovered.json"), meta).unwrap();

        let mut app = make_app();
        app.discover_orc_agents_from(dir.path());

        assert_eq!(app.agents.len(), 1);
        assert_eq!(app.agents[0].name, "discovered");
        assert_eq!(app.agents[0].task_description, "do stuff");
        assert_eq!(app.agents[0].pane.pane, 5);
    }

    #[test]
    fn test_discover_skips_existing() {
        let dir = TempDir::new().unwrap();
        let meta = r#"{
            "name": "existing",
            "task": "task",
            "session": "orc",
            "window": 0,
            "pane": 1,
            "worktree": "/tmp/wt"
        }"#;
        fs::write(dir.path().join("existing.json"), meta).unwrap();

        let mut app = make_app();
        app.agents.push(make_agent("existing"));
        app.discover_orc_agents_from(dir.path());

        assert_eq!(app.agents.len(), 1); // not duplicated
    }

    #[test]
    fn test_discover_skips_invalid_json() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("bad.json"), "not json").unwrap();
        fs::write(dir.path().join("readme.txt"), "ignored").unwrap();

        let mut app = make_app();
        app.discover_orc_agents_from(dir.path());

        assert_eq!(app.agents.len(), 0);
    }

    #[test]
    fn test_discover_empty_dir() {
        let dir = TempDir::new().unwrap();
        let mut app = make_app();
        app.discover_orc_agents_from(dir.path());
        assert_eq!(app.agents.len(), 0);
    }

    #[test]
    fn test_discover_nonexistent_dir() {
        let mut app = make_app();
        app.discover_orc_agents_from(std::path::Path::new("/nonexistent/path"));
        assert_eq!(app.agents.len(), 0); // should not panic
    }
}
