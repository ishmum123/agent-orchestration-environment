// App state container for orc v2.
//
// Two kinds of state:
// - Authoritative: projections of StateManager data, updated via broadcast
// - UI-local: focused tab, scroll, modals, pending input — ephemeral

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crate::input_attachments::AttachmentSet;
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
    /// A directive sent into a worker by orc (initial task, or follow-up
    /// `instruct_session`). Visually distinct from a UserText so the user
    /// can tell who said what.
    OrcInstruction(String),
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
    /// Call-to-action notifications (e.g. "foo is ready for your review").
    /// Rendered brighter than System so the user can read them.
    Notify(String),
    TurnEnd {
        cost_usd: Option<f64>,
    },
    /// Post-turn rollup of consecutive exploratory tool calls (Read, Grep,
    /// LS, Glob). Inserted by `collapse_last_turn` when a turn ends.
    Exploration {
        reads: u32,
        greps: u32,
        lists: u32,
        globs: u32,
    },
}

/// Encode a LogEntry into a (kind, payload) pair for DB persistence.
/// Payload is JSON so multi-field variants round-trip cleanly.
pub fn entry_to_kind_payload(entry: &LogEntry) -> (String, String) {
    use serde_json::json;
    match entry {
        LogEntry::UserText(t) => ("user".into(), json!({ "t": t }).to_string()),
        LogEntry::AssistantText(t) => ("assistant".into(), json!({ "t": t }).to_string()),
        LogEntry::Thinking(t) => ("thinking".into(), json!({ "t": t }).to_string()),
        LogEntry::OrcInstruction(t) => ("orc_instruction".into(), json!({ "t": t }).to_string()),
        LogEntry::ToolUse { name, input_summary } => (
            "tool_use".into(),
            json!({ "name": name, "input": input_summary }).to_string(),
        ),
        LogEntry::ToolResult { text, is_error } => (
            "tool_result".into(),
            json!({ "text": text, "is_error": is_error }).to_string(),
        ),
        LogEntry::System(t) => ("system".into(), json!({ "t": t }).to_string()),
        LogEntry::Notify(t) => ("notify".into(), json!({ "t": t }).to_string()),
        LogEntry::TurnEnd { cost_usd } => (
            "turn_end".into(),
            json!({ "cost_usd": cost_usd }).to_string(),
        ),
        LogEntry::Exploration { reads, greps, lists, globs } => (
            "exploration".into(),
            json!({ "reads": reads, "greps": greps, "lists": lists, "globs": globs }).to_string(),
        ),
    }
}

/// Decode a (kind, payload) pair back into a LogEntry. Unknown kinds
/// fall back to a System entry preserving the raw payload.
pub fn kind_payload_to_entry(kind: &str, payload: &str) -> LogEntry {
    let v: serde_json::Value = serde_json::from_str(payload).unwrap_or(serde_json::Value::Null);
    let s = |k: &str| -> String { v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string() };
    let b = |k: &str| -> bool { v.get(k).and_then(|x| x.as_bool()).unwrap_or(false) };
    let u = |k: &str| -> u32 { v.get(k).and_then(|x| x.as_u64()).unwrap_or(0) as u32 };
    let f = |k: &str| -> Option<f64> { v.get(k).and_then(|x| x.as_f64()) };
    match kind {
        "user" => LogEntry::UserText(s("t")),
        "assistant" => LogEntry::AssistantText(s("t")),
        "thinking" => LogEntry::Thinking(s("t")),
        "orc_instruction" => LogEntry::OrcInstruction(s("t")),
        "tool_use" => LogEntry::ToolUse {
            name: s("name"),
            input_summary: s("input"),
        },
        "tool_result" => LogEntry::ToolResult {
            text: s("text"),
            is_error: b("is_error"),
        },
        "system" => LogEntry::System(s("t")),
        "notify" => LogEntry::Notify(s("t")),
        "turn_end" => LogEntry::TurnEnd { cost_usd: f("cost_usd") },
        "exploration" => LogEntry::Exploration {
            reads: u("reads"),
            greps: u("greps"),
            lists: u("lists"),
            globs: u("globs"),
        },
        _ => LogEntry::System(payload.to_string()),
    }
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
    /// True if the most recent ToolUse for this agent was an orc-served
    /// MCP call we hid from the log. Used to suppress the matching
    /// ToolResult that follows.
    pub skip_next_tool_result: bool,
    /// Auto-follow tail. New entries scroll the view to the bottom while
    /// true. Manual scroll-up sets this false; `G` re-enables.
    pub stick_to_bottom: bool,
    /// True while the agent is in a thinking phase. Drives the spinner
    /// shown at the tail of the events log. Cleared by the next text /
    /// tool / turn-end event.
    pub is_thinking: bool,
    /// Latest assistant turn's full context-window occupancy in tokens
    /// (input + cache_read + cache_creation). None until first turn.
    pub last_context_tokens: Option<u64>,
}

// ---------------------------------------------------------------------------
// Orc view — uses the same event_log model.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct OrcView {
    pub event_log: Vec<LogEntry>,
    pub alive: bool,
    /// Configured model alias (e.g. "opus"). Shown in the agents panel.
    pub model: String,
    /// See `SessionView::skip_next_tool_result`.
    pub skip_next_tool_result: bool,
    /// Whether the `[orc model: …]` line has been pushed once already.
    /// Subsequent System events with a model field are ignored.
    pub model_announced: bool,
    /// Auto-follow tail (see `SessionView::stick_to_bottom`).
    pub stick_to_bottom: bool,
    /// See `SessionView::is_thinking`.
    pub is_thinking: bool,
    /// See `SessionView::last_context_tokens`.
    pub last_context_tokens: Option<u64>,
}

impl OrcView {
    pub fn new() -> Self {
        Self {
            event_log: Vec::new(),
            alive: true,
            model: String::new(),
            skip_next_tool_result: false,
            model_announced: false,
            stick_to_bottom: true,
            is_thinking: false,
            last_context_tokens: None,
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
        attachments: AttachmentSet,
    },
    AskUser {
        session_id: String,
        question_id: String,
        question: String,
        context: Option<String>,
        buffer: String,
        /// When true, modal is hidden so the user can scroll the chat
        /// behind it. `Tab` toggles. State (buffer, question) is preserved.
        hidden: bool,
        attachments: AttachmentSet,
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
    /// Offered after the user submits a review approval. Enter merges
    /// the worker's branch into `target` (the project's main branch);
    /// Esc leaves the branch alone.
    ConfirmMerge {
        session_id: String,
        name: String,
        branch: String,
        target: String,
    },
    /// Shown on startup when one or more workers were Running at the
    /// previous orc exit. The user picks whether to restart them all
    /// (their claude processes did not survive).
    ResumeRunning {
        names: Vec<String>,
    },
    /// Offered after a successful merge when the project has an
    /// `origin` remote. Enter pushes; Esc skips.
    ConfirmPush {
        branch: String,
        target: String,
    },
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
    /// Session ids that reached a terminal state (Done / Failed) and
    /// should be fully cleaned up (kill child, drop worktree, delete db
    /// rows). Drained by the main loop after each frame so UI logic
    /// stays sync-only.
    pub pending_cleanup: Vec<String>,
    /// First half of a `gg` chord (vim-style jump-to-top). Set by `g`,
    /// consumed by next key. Cleared on any other key.
    pub pending_g: bool,

    /// Persistent scratch claude overlay. Spawned on the first `?` and
    /// kept alive across overlay open/close until orc quits. Sealed off
    /// from orc's trust root — its events never enter `orc_view`.
    pub backchannel: Option<crate::backchannel::Backchannel>,
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
            pending_g: false,
            pending_cleanup: Vec::new(),
            backchannel: None,
        }
    }

    /// Set tail-follow flag for the given tab.
    pub fn set_stick(&mut self, tab: TabId, stick: bool) {
        match tab {
            TabId::Orc => self.orc_view.stick_to_bottom = stick,
            TabId::Worker(idx) => {
                if let Some(sv) = self.sessions.get_mut(idx) {
                    sv.stick_to_bottom = stick;
                }
            }
        }
    }

    /// Read tail-follow flag for the given tab.
    pub fn stick_to_bottom(&self, tab: TabId) -> bool {
        match tab {
            TabId::Orc => self.orc_view.stick_to_bottom,
            TabId::Worker(idx) => self
                .sessions
                .get(idx)
                .map(|sv| sv.stick_to_bottom)
                .unwrap_or(true),
        }
    }

    /// Set scroll position for a tab.
    pub fn set_scroll(&mut self, tab: TabId, pos: usize) {
        self.scroll.insert(tab, pos);
    }

    /// Collapse the just-completed turn's Read/Grep/LS/Glob ToolUse+Result
    /// pairs into single `Exploration` entries, preserving the order of
    /// other entries. Call after pushing `LogEntry::TurnEnd`.
    pub fn collapse_last_turn(&mut self, tab: TabId) {
        let log: &mut Vec<LogEntry> = match tab {
            TabId::Orc => &mut self.orc_view.event_log,
            TabId::Worker(idx) => match self.sessions.get_mut(idx) {
                Some(sv) => &mut sv.event_log,
                None => return,
            },
        };
        if log.is_empty() {
            return;
        }
        let end = log.len() - 1;
        if !matches!(log[end], LogEntry::TurnEnd { .. }) {
            return;
        }
        // Find the previous TurnEnd (or use 0).
        let mut start = 0;
        for i in (0..end).rev() {
            if matches!(log[i], LogEntry::TurnEnd { .. }) {
                start = i + 1;
                break;
            }
        }

        let mut rebuilt: Vec<LogEntry> = Vec::with_capacity(end - start);
        let mut acc = (0u32, 0u32, 0u32, 0u32); // reads, greps, lists, globs
        let flush = |rebuilt: &mut Vec<LogEntry>, acc: &mut (u32, u32, u32, u32)| {
            if acc.0 + acc.1 + acc.2 + acc.3 > 0 {
                rebuilt.push(LogEntry::Exploration {
                    reads: acc.0,
                    greps: acc.1,
                    lists: acc.2,
                    globs: acc.3,
                });
                *acc = (0, 0, 0, 0);
            }
        };

        let mut i = start;
        while i < end {
            // Detect (exploratory ToolUse, ToolResult) pair.
            if i + 1 < end {
                if let LogEntry::ToolUse { name, .. } = &log[i] {
                    let kind = match name.as_str() {
                        "Read" => Some(0),
                        "Grep" => Some(1),
                        "LS" => Some(2),
                        "Glob" => Some(3),
                        _ => None,
                    };
                    if let Some(k) = kind {
                        if matches!(log[i + 1], LogEntry::ToolResult { .. }) {
                            match k {
                                0 => acc.0 += 1,
                                1 => acc.1 += 1,
                                2 => acc.2 += 1,
                                _ => acc.3 += 1,
                            }
                            i += 2;
                            continue;
                        }
                    }
                }
            }
            // Non-exploratory: flush accumulated rollup, then keep entry.
            let mut local_acc = acc;
            flush(&mut rebuilt, &mut local_acc);
            acc = local_acc;
            rebuilt.push(log[i].clone());
            i += 1;
        }
        let mut local_acc = acc;
        flush(&mut rebuilt, &mut local_acc);

        // Splice rebuilt into log[start..end], leaving the TurnEnd in place.
        log.splice(start..end, rebuilt);
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
            skip_next_tool_result: false,
            stick_to_bottom: true,
            is_thinking: false,
            last_context_tokens: None,
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
    /// Also persist via the StateHandle if available so the events box
    /// survives an orc restart for Running / AwaitingReview workers.
    pub fn push_log(&mut self, session_id: &str, entry: LogEntry) {
        if let Some(&idx) = self.session_index.get(session_id) {
            let log = &mut self.sessions[idx].event_log;
            log.push(entry.clone());
            if log.len() > 5000 {
                let drop = log.len() - 5000;
                log.drain(0..drop);
            }
        }
        if let Some(handle) = &self.state_handle {
            let (kind, payload) = entry_to_kind_payload(&entry);
            let h = handle.clone();
            let sid = session_id.to_string();
            tokio::spawn(async move {
                let _ = h
                    .send(crate::state::StateCommand::AppendEvent {
                        session_id: sid,
                        kind,
                        payload,
                    })
                    .await;
            });
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
            .num_seconds()
            .max(0) as u64;
        format_elapsed(elapsed)
    }
}

/// Compact elapsed-time formatter:
///   <60s          → "Ns"        e.g. "23s"
///   <60min        → "Mm"        e.g. "1m", "47m"   (no seconds — once a
///                                                    minute has passed,
///                                                    seconds are noise)
///   <24h          → "HhMm"      e.g. "3h05m"
///   ≥24h          → "Dd"        e.g. "2d"
pub fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    let rem_mins = mins % 60;
    if hours < 24 {
        return format!("{hours}h{rem_mins:02}m");
    }
    let days = hours / 24;
    format!("{days}d")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{BlockKind, SessionMode};

    #[test]
    fn elapsed_format_seconds() {
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(59), "59s");
    }

    #[test]
    fn elapsed_format_minutes_drops_seconds() {
        // Once a minute has passed, seconds are noise — show only the
        // minute count regardless of remainder.
        assert_eq!(format_elapsed(60), "1m");
        assert_eq!(format_elapsed(83), "1m");
        assert_eq!(format_elapsed(599), "9m");
        assert_eq!(format_elapsed(600), "10m");
        assert_eq!(format_elapsed(3540), "59m");
    }

    #[test]
    fn elapsed_format_hours() {
        assert_eq!(format_elapsed(3600), "1h00m");
        assert_eq!(format_elapsed(7325), "2h02m");
    }

    #[test]
    fn elapsed_format_days() {
        assert_eq!(format_elapsed(86_400), "1d");
        assert_eq!(format_elapsed(2 * 86_400 + 3600), "2d");
    }

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
