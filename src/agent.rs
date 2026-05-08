use ratatui::style::Color;
use std::path::PathBuf;
use std::time::Instant;

use crate::claude::ClaudeProcess;

#[derive(Debug, Clone, PartialEq)]
pub enum AgentState {
    Working,
    Done,
    Error,
}

impl AgentState {
    pub fn label(&self) -> &str {
        match self {
            AgentState::Working => "working",
            AgentState::Done => "done",
            AgentState::Error => "error",
        }
    }

    pub fn color(&self) -> Color {
        match self {
            AgentState::Working => Color::Green,
            AgentState::Done => Color::Cyan,
            AgentState::Error => Color::Red,
        }
    }

    pub fn icon(&self) -> &str {
        match self {
            AgentState::Working => "\u{25cf}",  // ●
            AgentState::Done => "\u{2713}",     // ✓
            AgentState::Error => "\u{2717}",    // ✗
        }
    }
}

/// A single entry in an agent's output log.
#[derive(Debug, Clone)]
pub enum OutputEntry {
    Text(String),
    ToolUse { name: String, input: String },
    Result { text: String, is_error: bool },
    UserInput(String),
}

/// Structured output log, replacing raw terminal capture.
pub struct OutputLog {
    entries: Vec<OutputEntry>,
}

impl OutputLog {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn push_text(&mut self, text: &str) {
        self.entries.push(OutputEntry::Text(text.to_string()));
    }

    pub fn push_tool_use(&mut self, name: &str, input: &str) {
        self.entries.push(OutputEntry::ToolUse {
            name: name.to_string(),
            input: input.to_string(),
        });
    }

    pub fn push_result(&mut self, text: &str, is_error: bool) {
        self.entries.push(OutputEntry::Result {
            text: text.to_string(),
            is_error,
        });
    }

    pub fn push_user_input(&mut self, text: &str) {
        self.entries.push(OutputEntry::UserInput(text.to_string()));
    }

    pub fn recent(&self, n: usize) -> &[OutputEntry] {
        let start = self.entries.len().saturating_sub(n);
        &self.entries[start..]
    }

    pub fn all_entries(&self) -> &[OutputEntry] {
        &self.entries
    }
}

pub struct Agent {
    pub name: String,
    pub task_description: String,
    pub process: Option<ClaudeProcess>,
    pub worktree: PathBuf,
    pub state: AgentState,
    pub output: OutputLog,
    pub created_at: Instant,
    pub cost_usd: f64,
    /// When the agent finished (Done/Error). Used for pruning.
    pub done_at: Option<Instant>,
    /// orc_prompt_count snapshot when this agent became a prune candidate.
    /// Agent is pruned when orc_prompt_count exceeds this value.
    pub prune_after_orc_prompt: Option<u64>,
    /// Latest context size in tokens, from stream-json usage data.
    pub context_tokens: u64,
    /// Context threshold already reported to orc (avoids repeat alerts).
    pub context_warned: ContextWarning,
}

/// Tracks which context size warnings have been sent to the orc.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ContextWarning {
    #[default]
    None,
    /// Warned at ~50K tokens — orc should start planning offload.
    Warned50k,
    /// Warned at ~75K tokens — orc must act now.
    Warned75k,
}

impl Agent {
    pub fn new(
        name: String,
        task_description: String,
        process: ClaudeProcess,
        worktree: PathBuf,
    ) -> Self {
        Self {
            name,
            task_description,
            process: Some(process),
            worktree,
            state: AgentState::Working,
            output: OutputLog::new(),
            created_at: Instant::now(),
            cost_usd: 0.0,
            done_at: None,
            prune_after_orc_prompt: None,
            context_tokens: 0,
            context_warned: ContextWarning::None,
        }
    }

    /// For tests that don't need a real process.
    #[cfg(test)]
    pub fn new_without_process(
        name: String,
        task_description: String,
        worktree: PathBuf,
    ) -> Self {
        Self {
            name,
            task_description,
            process: None,
            worktree,
            state: AgentState::Done,
            output: OutputLog::new(),
            created_at: Instant::now(),
            cost_usd: 0.0,
            done_at: None,
            prune_after_orc_prompt: None,
            context_tokens: 0,
            context_warned: ContextWarning::None,
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

    pub fn kill(&mut self) {
        if let Some(ref mut proc) = self.process {
            proc.kill();
        }
        self.process = None;
    }
}

/// Generate a short slug from a task description.
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_state_labels() {
        assert_eq!(AgentState::Working.label(), "working");
        assert_eq!(AgentState::Done.label(), "done");
        assert_eq!(AgentState::Error.label(), "error");
    }

    #[test]
    fn test_state_icons_are_single_char() {
        for state in [AgentState::Working, AgentState::Done, AgentState::Error] {
            assert_eq!(state.icon().chars().count(), 1);
        }
    }

    #[test]
    fn test_elapsed_display_format() {
        let agent = Agent::new_without_process(
            "test".to_string(),
            "test task".to_string(),
            std::path::PathBuf::from("/tmp"),
        );
        let display = agent.elapsed_display();
        assert!(display.ends_with('s'), "expected seconds format, got: {}", display);
    }

    #[test]
    fn test_output_log_push_text() {
        let mut log = OutputLog::new();
        log.push_text("hello world");
        let entries = log.recent(1);
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            OutputEntry::Text(t) => assert_eq!(t, "hello world"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn test_output_log_push_tool() {
        let mut log = OutputLog::new();
        log.push_tool_use("Bash", "ls -la");
        let entries = log.recent(10);
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            OutputEntry::ToolUse { name, input } => {
                assert_eq!(name, "Bash");
                assert_eq!(input, "ls -la");
            }
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn test_output_log_recent_capped() {
        let mut log = OutputLog::new();
        for i in 0..200 {
            log.push_text(&format!("line {}", i));
        }
        let recent = log.recent(5);
        assert_eq!(recent.len(), 5);
    }
}
