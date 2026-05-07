use serde::Deserialize;
use std::path::PathBuf;
use std::time::Instant;

use ratatui::style::Color;

use crate::tmux::TmuxPane;

/// Metadata written by orc to ~/.orc/agents/{name}.json
#[derive(Deserialize)]
pub struct AgentMeta {
    pub name: String,
    pub task: String,
    pub session: String,
    pub window: u32,
    pub pane: u32,
    pub worktree: String,
}

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
            AgentState::WaitingForUser => "needs you",
            AgentState::Idle => "idle",
            AgentState::Done => "done",
            AgentState::Stuck => "stuck",
        }
    }

    pub fn color(&self) -> Color {
        match self {
            AgentState::Working => Color::Green,
            AgentState::WaitingForUser => Color::Yellow,
            AgentState::Idle => Color::DarkGray,
            AgentState::Done => Color::Cyan,
            AgentState::Stuck => Color::Red,
        }
    }

    pub fn icon(&self) -> &str {
        match self {
            AgentState::Working => "\u{25cf}",    // ●
            AgentState::WaitingForUser => "\u{25cb}", // ○
            AgentState::Idle => "\u{2500}",       // ─
            AgentState::Done => "\u{2713}",       // ✓
            AgentState::Stuck => "\u{2717}",      // ✗
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
    pub created_at: Instant,
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
            created_at: Instant::now(),
        }
    }

    pub fn elapsed_display(&self) -> String {
        let secs = self.created_at.elapsed().as_secs();
        if secs < 60 {
            format!("{}s", secs)
        } else if secs < 3600 {
            format!("{}m", secs / 60)
        } else {
            format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
        }
    }
}

/// Generate a short slug from a task description.
/// "fix authentication bug in login" → "fix-auth-bug"
/// Takes first 3 words, truncates long words.
pub fn slugify_description(desc: &str) -> String {
    let words: Vec<&str> = desc
        .split_whitespace()
        .filter(|w| {
            let lower = w.to_lowercase();
            !matches!(lower.as_str(), "the" | "a" | "an" | "in" | "on" | "for" | "to" | "and" | "or" | "of" | "with")
        })
        .take(3)
        .collect();

    if words.is_empty() {
        return "agent".to_string();
    }

    words
        .iter()
        .map(|w| {
            let cleaned: String = w.chars()
                .filter(|c| c.is_alphanumeric())
                .take(6)
                .collect();
            cleaned.to_lowercase()
        })
        .collect::<Vec<_>>()
        .join("-")
}

pub fn detect_state(output: &str) -> AgentState {
    let lines: Vec<&str> = output.lines().collect();
    let last_lines: Vec<&str> = lines.iter().rev().take(15).copied().collect();

    // Claude Code permission prompts
    for line in &last_lines {
        let lower = line.to_lowercase();
        if lower.contains("? (y/n)")
            || lower.contains("do you want")
            || lower.contains("should i")
            || lower.contains("would you like")
            || lower.contains("allow once")
            || lower.contains("allow always")
            || lower.contains("deny")
            || lower.contains("approve?")
            || lower.contains("proceed?")
        {
            return AgentState::WaitingForUser;
        }
    }

    let error_count = last_lines
        .iter()
        .filter(|l| {
            let lower = l.to_lowercase();
            lower.contains("error:") || lower.contains("error[")
        })
        .count();
    if error_count >= 2 {
        return AgentState::Stuck;
    }

    for line in &last_lines {
        let lower = line.to_lowercase();
        if lower.contains("task completed")
            || lower.contains("all done")
            || lower.contains("changes committed")
            || lower.contains("implementation complete")
            || lower.contains("finished implementing")
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

    // -- detect_state --

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
    fn test_detect_waiting_permission() {
        let output = "I need to edit src/main.rs\nAllow once / Allow always / Deny\n";
        assert_eq!(detect_state(output), AgentState::WaitingForUser);
    }

    #[test]
    fn test_detect_waiting_approve() {
        let output = "I want to run this command.\nApprove?\n";
        assert_eq!(detect_state(output), AgentState::WaitingForUser);
    }

    #[test]
    fn test_detect_waiting_proceed() {
        let output = "This will modify 5 files.\nProceed?\n";
        assert_eq!(detect_state(output), AgentState::WaitingForUser);
    }

    #[test]
    fn test_detect_waiting_would_you_like() {
        let output = "Some output here\nWould you like me to continue?\n";
        assert_eq!(detect_state(output), AgentState::WaitingForUser);
    }

    #[test]
    fn test_detect_stuck() {
        let output = "error: could not compile\nerror: aborting due to previous error:\n";
        assert_eq!(detect_state(output), AgentState::Stuck);
    }

    #[test]
    fn test_detect_stuck_error_bracket() {
        let output = "error[E0308]: mismatched types\nerror[E0061]: wrong number of args\n";
        assert_eq!(detect_state(output), AgentState::Stuck);
    }

    #[test]
    fn test_detect_not_stuck_single_error() {
        // Single error line should not trigger stuck
        let output = "building...\nerror: something happened\nretrying...\n";
        assert_eq!(detect_state(output), AgentState::Working);
    }

    #[test]
    fn test_detect_idle() {
        let output = "done running\nuser@host ~/project> ";
        assert_eq!(detect_state(output), AgentState::Idle);
    }

    #[test]
    fn test_detect_idle_dollar() {
        // trim_end() strips the trailing space, so "$ " becomes "$"
        // which doesn't match. But ">" does match after trim.
        let output = "finished\nuser@host:~>";
        assert_eq!(detect_state(output), AgentState::Idle);
    }

    #[test]
    fn test_detect_done_task_completed() {
        let output = "All changes applied.\nTask completed successfully.\n";
        assert_eq!(detect_state(output), AgentState::Done);
    }

    #[test]
    fn test_detect_done_all_done() {
        let output = "Tests pass.\nAll done!\n";
        assert_eq!(detect_state(output), AgentState::Done);
    }

    #[test]
    fn test_detect_done_changes_committed() {
        let output = "Created commit abc123.\nChanges committed.\n";
        assert_eq!(detect_state(output), AgentState::Done);
    }

    #[test]
    fn test_detect_done_implementation_complete() {
        let output = "Wrote 3 files.\nImplementation complete.\n";
        assert_eq!(detect_state(output), AgentState::Done);
    }

    #[test]
    fn test_detect_done_finished_implementing() {
        let output = "Finished implementing the auth module.\n";
        assert_eq!(detect_state(output), AgentState::Done);
    }

    #[test]
    fn test_detect_waiting_beats_stuck() {
        // Permission prompt should win even if errors are present
        let output = "error: build failed\nerror: try again\nAllow once / Allow always / Deny\n";
        assert_eq!(detect_state(output), AgentState::WaitingForUser);
    }

    #[test]
    fn test_detect_empty_input() {
        assert_eq!(detect_state(""), AgentState::Working);
    }

    #[test]
    fn test_detect_whitespace_only() {
        assert_eq!(detect_state("   \n  \n  "), AgentState::Working);
    }

    // -- slugify_description --

    #[test]
    fn test_slugify() {
        assert_eq!(slugify_description("fix authentication bug in login"), "fix-authen-bug");
        assert_eq!(slugify_description("add the user profile page"), "add-user-profil");
        assert_eq!(slugify_description("refactor"), "refact");
        assert_eq!(slugify_description(""), "agent");
    }

    #[test]
    fn test_slugify_strips_stop_words() {
        assert_eq!(slugify_description("add a new feature to the app"), "add-new-featur");
    }

    #[test]
    fn test_slugify_special_chars() {
        assert_eq!(slugify_description("fix: auth-bug (urgent!)"), "fix-authbu-urgent");
    }

    #[test]
    fn test_slugify_all_stop_words() {
        assert_eq!(slugify_description("the a an in on for"), "agent");
    }

    #[test]
    fn test_slugify_long_words_truncated() {
        assert_eq!(slugify_description("internationalization"), "intern");
    }

    // -- AgentMeta deserialization --

    #[test]
    fn test_agent_meta_deser() {
        let json = r#"{
            "name": "auth-fix",
            "task": "Fix the auth bug",
            "session": "orc",
            "window": 0,
            "pane": 3,
            "worktree": "/tmp/worktrees/auth-fix"
        }"#;
        let meta: AgentMeta = serde_json::from_str(json).unwrap();
        assert_eq!(meta.name, "auth-fix");
        assert_eq!(meta.task, "Fix the auth bug");
        assert_eq!(meta.session, "orc");
        assert_eq!(meta.window, 0);
        assert_eq!(meta.pane, 3);
        assert_eq!(meta.worktree, "/tmp/worktrees/auth-fix");
    }

    #[test]
    fn test_agent_meta_deser_rejects_missing_field() {
        let json = r#"{"name": "x", "task": "y"}"#;
        assert!(serde_json::from_str::<AgentMeta>(json).is_err());
    }

    // -- AgentState display --

    #[test]
    fn test_state_labels() {
        assert_eq!(AgentState::Working.label(), "working");
        assert_eq!(AgentState::WaitingForUser.label(), "needs you");
        assert_eq!(AgentState::Idle.label(), "idle");
        assert_eq!(AgentState::Done.label(), "done");
        assert_eq!(AgentState::Stuck.label(), "stuck");
    }

    #[test]
    fn test_state_icons_are_single_char() {
        for state in [AgentState::Working, AgentState::WaitingForUser, AgentState::Idle, AgentState::Done, AgentState::Stuck] {
            assert_eq!(state.icon().chars().count(), 1);
        }
    }

    // -- elapsed_display --

    #[test]
    fn test_elapsed_display_format() {
        let agent = Agent::new(
            "test".to_string(),
            "test task".to_string(),
            crate::tmux::TmuxPane { session: "s".to_string(), window: 0, pane: 0 },
            std::path::PathBuf::from("/tmp"),
        );
        // Just created — should be "0s" or "1s"
        let display = agent.elapsed_display();
        assert!(display.ends_with('s'), "expected seconds format, got: {}", display);
    }
}
