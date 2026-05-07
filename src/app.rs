use crate::agent::Agent;
use crate::tmux::{self, TmuxSession};
use anyhow::Result;
use std::time::Instant;

/// Dashboard interaction modes
pub enum AppMode {
    /// Viewing the dashboard, navigating agents
    Dashboard,
    /// Typing an inline prompt (for [t]ell or [n]ew)
    Input {
        prompt_label: String,
        callback: InputCallback,
    },
    /// User is attached to a tmux pane — CLI is suspended
    Attached,
    /// Viewing layered status summaries
    Status,
}

/// What to do with inline input after Enter
pub enum InputCallback {
    TellAgent,
    NewAgent,
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
    pub orc_pane: Option<tmux::TmuxPane>,
}

impl App {
    pub fn new(session_name: &str, project_dir: &str) -> Self {
        Self {
            session: TmuxSession::new(session_name),
            agents: Vec::new(),
            selected: 0,
            mode: AppMode::Dashboard,
            input_buf: String::new(),
            should_quit: false,
            project_dir: project_dir.to_string(),
            last_poll: Instant::now(),
            status_line: String::new(),
            orc_pane: None,
        }
    }

    pub fn selected_agent(&self) -> Option<&Agent> {
        self.agents.get(self.selected)
    }

    pub fn select_next(&mut self) {
        if !self.agents.is_empty() {
            self.selected = (self.selected + 1) % self.agents.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.agents.is_empty() {
            self.selected = if self.selected == 0 {
                self.agents.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    /// Poll all agent panes for state updates
    pub fn poll_agents(&mut self) -> Result<()> {
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
}
