use std::path::PathBuf;
use std::time::Instant;

use ratatui::style::Color;

use crate::tmux::TmuxPane;

#[derive(Debug, Clone, PartialEq)]
pub enum AgentState {
    Working,
    WaitingForUser,
    Idle,
    Done,
    Stuck,
}

impl AgentState {
    pub fn label(&self) -> &str {
        match self {
            AgentState::Working => "working",
            AgentState::WaitingForUser => "waiting for you",
            AgentState::Idle => "idle",
            AgentState::Done => "done",
            AgentState::Stuck => "stuck",
        }
    }

    pub fn color(&self) -> Color {
        match self {
            AgentState::Working => Color::Green,
            AgentState::WaitingForUser => Color::Yellow,
            AgentState::Idle => Color::Gray,
            AgentState::Done => Color::Blue,
            AgentState::Stuck => Color::Red,
        }
    }
}

pub struct Agent {
    pub name: String,
    pub task_description: String,
    pub pane: TmuxPane,
    pub worktree: PathBuf,
    pub state: AgentState,
    pub last_output: String,
    pub locked: bool,
    pub last_poll: Instant,
}

impl Agent {
    pub fn new(name: String, task_description: String, pane: TmuxPane, worktree: PathBuf) -> Self {
        Self {
            name,
            task_description,
            pane,
            worktree,
            state: AgentState::Working,
            last_output: String::new(),
            locked: false,
            last_poll: Instant::now(),
        }
    }
}

pub fn detect_state(output: &str) -> AgentState {
    let lines: Vec<&str> = output.lines().collect();
    let last_lines: Vec<&str> = lines.iter().rev().take(10).copied().collect();

    for line in &last_lines {
        let lower = line.to_lowercase();
        if lower.contains("? (y/n)")
            || lower.contains("do you want")
            || lower.contains("should i")
            || lower.contains("would you like")
        {
            return AgentState::WaitingForUser;
        }
    }

    let error_count = last_lines
        .iter()
        .filter(|l| l.to_lowercase().contains("error:"))
        .count();
    if error_count >= 2 {
        return AgentState::Stuck;
    }

    for line in &last_lines {
        let lower = line.to_lowercase();
        if lower.contains("task completed")
            || lower.contains("all done")
            || lower.contains("changes committed")
        {
            return AgentState::Done;
        }
    }

    let trimmed = output.trim_end();
    if trimmed.ends_with('>') || trimmed.ends_with("$ ") {
        return AgentState::Idle;
    }

    AgentState::Working
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_working() {
        let output = "compiling crate...\nrunning build step 3\n";
        assert_eq!(detect_state(output), AgentState::Working);
    }

    #[test]
    fn test_detect_waiting() {
        let output = "I found some issues.\nShould I fix them? (y/n)\n";
        assert_eq!(detect_state(output), AgentState::WaitingForUser);
    }

    #[test]
    fn test_detect_stuck() {
        let output = "error: could not compile\nerror: aborting due to previous error:\n";
        assert_eq!(detect_state(output), AgentState::Stuck);
    }

    #[test]
    fn test_detect_idle() {
        let output = "done running\nuser@host ~/project> ";
        assert_eq!(detect_state(output), AgentState::Idle);
    }
}
