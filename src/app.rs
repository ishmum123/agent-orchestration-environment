// App state container for orc v2.
//
// Two kinds of state:
// - Authoritative: projections of StateManager data, updated via broadcast
// - UI-local: focused tab, scroll, modals, pending input — ephemeral

use std::collections::{HashMap, VecDeque};
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
// Chat messages (orc tab)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub text: String,
    pub timestamp: Instant,
}

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
    pub pty_tail: VecDeque<String>,
    pub permissions: Vec<PermissionEntry>,
    pub tab_index: usize,
}

// ---------------------------------------------------------------------------
// Modal overlays
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Modal {
    NewTask {
        buffer: String,
    },
    AskUser {
        session_id: String,
        question: String,
        context: Option<String>,
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
    pub orc_chat: Vec<ChatMessage>,

    // UI-local state
    pub focused_tab: TabId,
    pub modal: Option<Modal>,
    pub scroll: HashMap<TabId, usize>,
    pub pending_input: String,
    pub show_graph: bool,
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
            orc_chat: Vec::new(),
            focused_tab: TabId::Orc,
            modal: None,
            scroll: HashMap::new(),
            pending_input: String::new(),
            show_graph: false,
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
            pty_tail: VecDeque::with_capacity(500),
            permissions: Vec::new(),
            tab_index: idx,
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

    /// Replace the pty_tail buffer with a snapshot of the currently visible
    /// pane. Called on every tick from the pane-capturer task — semantically
    /// "this is what the user would see right now if they attached."
    pub fn set_pty_tail(&mut self, session_id: &str, lines: Vec<String>) {
        if let Some(&idx) = self.session_index.get(session_id) {
            let tail = &mut self.sessions[idx].pty_tail;
            tail.clear();
            // Cap at 500 to match the existing buffer bound; the snapshot is
            // already only the visible pane height (~40 lines) but a user with
            // a tall terminal could exceed that if we ever switch to scrollback.
            let take_from = lines.len().saturating_sub(500);
            for line in lines.into_iter().skip(take_from) {
                tail.push_back(line);
            }
        }
    }

    pub fn append_pty_line(&mut self, session_id: &str, line: String) {
        if let Some(&idx) = self.session_index.get(session_id) {
            let tail = &mut self.sessions[idx].pty_tail;
            tail.push_back(line);
            if tail.len() > 500 {
                tail.pop_front();
            }
        }
    }

    // -- Chat management ------------------------------------------------------

    pub fn push_chat(&mut self, role: ChatRole, text: String) {
        self.orc_chat.push(ChatMessage {
            role,
            text,
            timestamp: Instant::now(),
        });
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
        }
    }

    #[test]
    fn tab_navigation_cycles() {
        let mut app = App::new(".");
        assert_eq!(app.focused_tab, TabId::Orc);

        // No workers — cycling stays on orc
        app.next_tab();
        assert_eq!(app.focused_tab, TabId::Orc);

        // Add workers
        app.add_session(test_session("s1", "worker1"));
        app.add_session(test_session("s2", "worker2"));

        app.next_tab();
        assert_eq!(app.focused_tab, TabId::Worker(0));
        app.next_tab();
        assert_eq!(app.focused_tab, TabId::Worker(1));
        app.next_tab();
        assert_eq!(app.focused_tab, TabId::Orc); // wraps

        app.prev_tab();
        assert_eq!(app.focused_tab, TabId::Worker(1)); // wraps back
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
        // Tab should adjust
        assert_eq!(app.focused_tab, TabId::Worker(1));
    }

    #[test]
    fn pty_tail_capacity() {
        let mut app = App::new(".");
        app.add_session(test_session("s1", "w1"));
        for i in 0..600 {
            app.append_pty_line("s1", format!("line {i}"));
        }
        assert_eq!(app.sessions[0].pty_tail.len(), 500);
        assert!(app.sessions[0].pty_tail.front().unwrap().contains("100"));
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
        assert_eq!(app.scroll_pos(TabId::Orc), 0); // doesn't underflow
    }

    #[test]
    fn chat_push() {
        let mut app = App::new(".");
        app.push_chat(ChatRole::User, "hello".to_string());
        app.push_chat(ChatRole::Orc, "hi back".to_string());
        assert_eq!(app.orc_chat.len(), 2);
        assert_eq!(app.orc_chat[0].role, ChatRole::User);
    }
}
