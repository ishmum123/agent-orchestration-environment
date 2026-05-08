// App state container for orc v2.
//
// Two kinds of state:
// - Authoritative: projections of StateManager data, updated via broadcast
// - UI-local: focused tab, scroll, modals, pending input — ephemeral

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crate::review::ReviewState;
use crate::session::{Session, SessionMode, SessionState};
use crate::state::StateHandle;

// ---------------------------------------------------------------------------
// Tab identifiers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TabId {
    Orc,
    Worker(usize), // index into sessions list
}

// ---------------------------------------------------------------------------
// Structured event log entry — used for both orc and worker tabs.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum LogEntry {
    UserText(String),
    AssistantText(String),
    Thinking(String),
    ToolUse {
        name: String,
        input_summary: String,
    },
    ToolResult {
        text: String,
        is_error: bool,
    },
    /// `[interrupted]`, `answered by orc`, `worker exited code 0`, etc.
    System(String),
    TurnEnd {
        cost_usd: Option<f64>,
    },
}

// Legacy chat-message API kept as a thin shim for existing callsites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Orc,
    System,
}

// ---------------------------------------------------------------------------
// Permission decision log entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PermissionEntry {
    pub timestamp: Instant,
    pub request: String,
    pub decision: String,
    pub decided_by: String,
}

// ---------------------------------------------------------------------------
// Session view — UI projection of authoritative state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SessionView {
    pub session: Session,
    pub event_log: Vec<LogEntry>,
    pub permissions: Vec<PermissionEntry>,
    pub tab_index: usize,
    pub claude_session_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Orc view — uses the same event_log model.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct OrcView {
    pub event_log: Vec<LogEntry>,
    pub alive: bool,
}

impl OrcView {
    pub fn new() -> Self {
        Self {
            event_log: Vec::new(),
            alive: true,
        }
    }

    pub fn push(&mut self, entry: LogEntry) {
        self.event_log.push(entry);
        if self.event_log.len() > 5000 {
            let drop = self.event_log.len() - 5000;
            self.event_log.drain(0..drop);
        }
    }
}

impl Default for OrcView {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Modal overlays
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Modal {
    NewTask {
        target: TabId,
        buffer: String,
    },
    AskUser {
        session_id: String,
        question_id: String,
        question: String,
        context: Option<String>,
        buffer: String,
    },
    Comment {
        session_id: String,
        file: String,
        line: usize,
        buffer: String,
    },
    ConfirmKill {
        session_id: String,
        name: String,
    },
    ConfirmQuit,
    Help,
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub struct App {
    pub project_dir: PathBuf,
    pub state_handle: Option<StateHandle>,

    // Authoritative projections (updated via broadcast)
    pub sessions: Vec<SessionView>,
    pub session_index: HashMap<String, usize>, // session_id -> sessions vec index
    pub orc_view: OrcView,

    // UI-local state
    pub focused_tab: TabId,
    pub modal: Option<Modal>,
    pub scroll: HashMap<TabId, usize>,
    pub pending_input: String,
    /// Per-session/orc one-line summary set via `current_summary` MCP tool.
    /// Key "orc" reserved for the orchestrator; otherwise session_id.
    pub summaries: HashMap<String, String>,
    pub review: Option<ReviewState>,
    pub started_at: Instant,

    pub should_quit: bool,
    pub tick: u64,
}

impl App {
    pub fn new(project_dir: &str) -> Self {
        Self {
            project_dir: PathBuf::from(project_dir),
            state_handle: None,
            sessions: Vec::new(),
            session_index: HashMap::new(),
            orc_view: OrcView::new(),
            focused_tab: TabId::Orc,
            modal: None,
            scroll: HashMap::new(),
            pending_input: String::new(),
            summaries: HashMap::new(),
            review: None,
            started_at: Instant::now(),
            should_quit: false,
            tick: 0,
        }
    }

    pub fn with_state_handle(mut self, handle: StateHandle) -> Self {
        self.state_handle = Some(handle);
        self
    }

    // -- Session management ---------------------------------------------------

    pub fn add_session(&mut self, session: Session) {
        let idx = self.sessions.len();
        self.session_index.insert(session.id.clone(), idx);
        self.sessions.push(SessionView {
            session,
            event_log: Vec::new(),
            permissions: Vec::new(),
            tab_index: idx,
            claude_session_id: None,
        });
    }

    pub fn update_session_state(&mut self, session_id: &str, new_state: SessionState) {
        if let Some(&idx) = self.session_index.get(session_id) {
            self.sessions[idx].session.state = new_state;
            self.sessions[idx].session.updated_at = chrono::Utc::now();
        }
    }

    pub fn update_session_mode(&mut self, session_id: &str, mode: SessionMode) {
        if let Some(&idx) = self.session_index.get(session_id) {
            self.sessions[idx].session.mode = mode;
        }
    }

    pub fn remove_session(&mut self, session_id: &str) {
        if let Some(&idx) = self.session_index.get(session_id) {
            self.sessions.remove(idx);
            self.session_index.remove(session_id);
            // Rebuild index
            self.session_index.clear();
            for (i, sv) in self.sessions.iter_mut().enumerate() {
                sv.tab_index = i;
                self.session_index.insert(sv.session.id.clone(), i);
            }
            // Fix focused tab if needed
            if let TabId::Worker(wi) = self.focused_tab {
                if wi >= self.sessions.len() {
                    self.focused_tab = if self.sessions.is_empty() {
                        TabId::Orc
                    } else {
                        TabId::Worker(self.sessions.len() - 1)
                    };
                }
            }
        }
    }

    /// Append a structured log entry to a worker's event log.
    pub fn push_log(&mut self, session_id: &str, entry: LogEntry) {
        if let Some(&idx) = self.session_index.get(session_id) {
            let log = &mut self.sessions[idx].event_log;
            log.push(entry);
            if log.len() > 5000 {
                let drop = log.len() - 5000;
                log.drain(0..drop);
            }
        }
    }

    /// Look up a one-line summary for an agent (orc or worker session_id).
    pub fn session_summary(&self, key: &str) -> Option<String> {
        self.summaries.get(key).cloned()
    }

    /// Update the summary for an agent (orc or worker session_id).
    pub fn set_session_summary(&mut self, key: String, summary: String) {
        self.summaries.insert(key, summary);
    }

    pub fn set_claude_session_id(&mut self, session_id: &str, sid: String) {
        if let Some(&idx) = self.session_index.get(session_id) {
            self.sessions[idx].claude_session_id = Some(sid);
        }
    }

    // -- Chat shim — keeps legacy push_chat callsites working by routing
    //    into the orc tab's event_log.
    pub fn push_chat(&mut self, role: ChatRole, text: String) {
        let entry = match role {
            ChatRole::User => LogEntry::UserText(text),
            ChatRole::Orc => LogEntry::AssistantText(text),
            ChatRole::System => LogEntry::System(text),
        };
        self.orc_view.push(entry);
    }

    // -- Tab navigation -------------------------------------------------------

    pub fn focused_session(&self) -> Option<&SessionView> {
        match self.focused_tab {
            TabId::Orc => None,
            TabId::Worker(idx) => self.sessions.get(idx),
        }
    }

    pub fn focus_tab(&mut self, tab: TabId) {
        self.focused_tab = tab;
    }

    pub fn next_tab(&mut self) {
        self.focused_tab = match self.focused_tab {
            TabId::Orc => {
                if self.sessions.is_empty() {
                    TabId::Orc
                } else {
                    TabId::Worker(0)
                }
            }
            TabId::Worker(idx) => {
                if idx + 1 < self.sessions.len() {
                    TabId::Worker(idx + 1)
                } else {
                    TabId::Orc
                }
            }
        };
    }

    pub fn prev_tab(&mut self) {
        self.focused_tab = match self.focused_tab {
            TabId::Orc => {
                if self.sessions.is_empty() {
                    TabId::Orc
                } else {
                    TabId::Worker(self.sessions.len() - 1)
                }
            }
            TabId::Worker(0) => TabId::Orc,
            TabId::Worker(idx) => TabId::Worker(idx - 1),
        };
    }

    pub fn tab_count(&self) -> usize {
        1 + self.sessions.len() // orc + workers
    }

    // -- Scroll ---------------------------------------------------------------

    pub fn scroll_pos(&self, tab: TabId) -> usize {
        self.scroll.get(&tab).copied().unwrap_or(0)
    }

    pub fn scroll_down(&mut self, tab: TabId, amount: usize) {
        let pos = self.scroll.entry(tab).or_insert(0);
        *pos = pos.saturating_add(amount);
    }

    pub fn scroll_up(&mut self, tab: TabId, amount: usize) {
        let pos = self.scroll.entry(tab).or_insert(0);
        *pos = pos.saturating_sub(amount);
    }

    // -- State badge helpers --------------------------------------------------

    pub fn state_badge(state: &SessionState) -> (&'static str, ratatui::style::Color) {
        use ratatui::style::Color;
        match state {
            SessionState::Running => ("◐", Color::Blue),
            SessionState::Blocked { kind, .. } => match kind {
                crate::session::BlockKind::Permission => ("?", Color::Yellow),
                crate::session::BlockKind::OrcDecision => ("⏸", Color::Yellow),
                crate::session::BlockKind::UserInput => ("!", Color::Red),
            },
            SessionState::AwaitingReview { .. } => ("◑", Color::Magenta),
            SessionState::Done { .. } => ("✓", Color::Green),
            SessionState::Failed { .. } => ("✗", Color::Red),
        }
    }

    pub fn elapsed_str(session: &Session) -> String {
        let elapsed = chrono::Utc::now()
            .signed_duration_since(session.created_at)
            .num_seconds();
        if elapsed < 60 {
            format!("{elapsed}s")
        } else {
            format!("{}m", elapsed / 60)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{BlockKind, SessionMode};

    fn test_session(id: &str, name: &str) -> Session {
        Session {
            id: id.to_string(),
            name: name.to_string(),
            task: "test task".to_string(),
            worktree_path: "/tmp/wt".to_string(),
            branch: "test".to_string(),
            base_commit: "abc123".to_string(),
            tmux_session: format!("orc-{name}"),
            state: SessionState::Running,
            mode: SessionMode::Watch,
            model: "sonnet".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ended_at: None,
            claude_session_id: None,
        }
    }

    #[test]
    fn tab_navigation_cycles() {
        let mut app = App::new(".");
        assert_eq!(app.focused_tab, TabId::Orc);

        app.next_tab();
        assert_eq!(app.focused_tab, TabId::Orc);

        app.add_session(test_session("s1", "worker1"));
        app.add_session(test_session("s2", "worker2"));

        app.next_tab();
        assert_eq!(app.focused_tab, TabId::Worker(0));
        app.next_tab();
        assert_eq!(app.focused_tab, TabId::Worker(1));
        app.next_tab();
        assert_eq!(app.focused_tab, TabId::Orc);

        app.prev_tab();
        assert_eq!(app.focused_tab, TabId::Worker(1));
    }

    #[test]
    fn add_and_remove_sessions() {
        let mut app = App::new(".");
        app.add_session(test_session("s1", "w1"));
        app.add_session(test_session("s2", "w2"));
        app.add_session(test_session("s3", "w3"));
        assert_eq!(app.sessions.len(), 3);

        app.focus_tab(TabId::Worker(2));
        app.remove_session("s2");
        assert_eq!(app.sessions.len(), 2);
        assert_eq!(app.focused_tab, TabId::Worker(1));
    }

    #[test]
    fn event_log_capacity() {
        let mut app = App::new(".");
        app.add_session(test_session("s1", "w1"));
        for i in 0..6000 {
            app.push_log("s1", LogEntry::System(format!("e{i}")));
        }
        assert_eq!(app.sessions[0].event_log.len(), 5000);
    }

    #[test]
    fn state_badges() {
        use ratatui::style::Color;
        assert_eq!(App::state_badge(&SessionState::Running), ("◐", Color::Blue));
        assert_eq!(
            App::state_badge(&SessionState::Blocked {
                kind: BlockKind::Permission,
                reason: String::new()
            }),
            ("?", Color::Yellow)
        );
        assert_eq!(
            App::state_badge(&SessionState::Done {
                summary: String::new()
            }),
            ("✓", Color::Green)
        );
    }

    #[test]
    fn scroll_operations() {
        let mut app = App::new(".");
        assert_eq!(app.scroll_pos(TabId::Orc), 0);
        app.scroll_down(TabId::Orc, 5);
        assert_eq!(app.scroll_pos(TabId::Orc), 5);
        app.scroll_up(TabId::Orc, 3);
        assert_eq!(app.scroll_pos(TabId::Orc), 2);
        app.scroll_up(TabId::Orc, 10);
        assert_eq!(app.scroll_pos(TabId::Orc), 0);
    }

    #[test]
    fn chat_push_routes_to_orc_view() {
        let mut app = App::new(".");
        app.push_chat(ChatRole::User, "hello".to_string());
        app.push_chat(ChatRole::Orc, "hi back".to_string());
        assert_eq!(app.orc_view.event_log.len(), 2);
        assert!(matches!(app.orc_view.event_log[0], LogEntry::UserText(_)));
    }
}
