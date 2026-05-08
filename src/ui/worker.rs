// Worker tab: per-agent detail view with session info, PTY preview, and permission log.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, SessionView};
use crate::session::SessionState;

/// Render the worker tab for a single session.
///
/// Layout (vertical):
///   1. Session info header  — 5 lines
///   2. PTY tail             — fills remaining space
///   3. Recent decisions     — min(8, permissions.len()+2) lines at bottom
pub fn render_worker(frame: &mut Frame, area: Rect, session_view: &SessionView) {
    let s = &session_view.session;
    let (badge, badge_color) = App::state_badge(&s.state);
    let elapsed = App::elapsed_str(s);

    // --- Layout split ---
    let decision_count = session_view.permissions.len().min(8);
    // Header for decisions line + entries + 1 blank = decision_count + 2, minimum 2 if empty
    let decisions_height = if decision_count > 0 {
        (decision_count + 2) as u16
    } else {
        0
    };

    let chunks = Layout::vertical([
        Constraint::Length(5),             // session info
        Constraint::Min(4),               // pty tail
        Constraint::Length(decisions_height),
    ])
    .split(area);

    render_session_info(frame, chunks[0], session_view, badge, badge_color, &elapsed);
    render_pty_tail(frame, chunks[1], session_view);
    if decisions_height > 0 {
        render_decisions(frame, chunks[2], session_view);
    }
}

fn state_label(state: &SessionState) -> &'static str {
    match state {
        SessionState::Running => "Running",
        SessionState::Blocked { .. } => "Blocked",
        SessionState::AwaitingReview { .. } => "AwaitingReview",
        SessionState::Done { .. } => "Done",
        SessionState::Failed { .. } => "Failed",
    }
}

/// Top block: name, model, state badge, worktree, branch, task.
fn render_session_info(
    frame: &mut Frame,
    area: Rect,
    sv: &SessionView,
    badge: &str,
    badge_color: Color,
    elapsed: &str,
) {
    let s = &sv.session;

    // Title line: "name · model ──── badge State elapsed"
    let title = Line::from(vec![
        Span::styled(&s.name, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" \u{b7} "),
        Span::styled(&s.model, Style::default().fg(Color::Cyan)),
    ]);
    let title_right = Line::from(vec![
        Span::styled(badge, Style::default().fg(badge_color).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(state_label(&s.state), Style::default().fg(badge_color)),
        Span::raw("  "),
        Span::styled(elapsed, Style::default().fg(Color::DarkGray)),
    ]);

    let block = Block::default()
        .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
        .title(title)
        .title_alignment(ratatui::layout::Alignment::Left);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Right-aligned badge on the title line (rendered inside inner, row 0)
    let badge_width = title_right.width() as u16;
    if badge_width < inner.width {
        let badge_area = Rect {
            x: inner.x + inner.width - badge_width,
            y: area.y, // on the border title line
            width: badge_width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(title_right), badge_area);
    }

    // Info lines inside block
    let lines = vec![
        Line::from(vec![
            Span::styled("  task:      ", Style::default().fg(Color::DarkGray)),
            Span::raw(truncate(&s.task, inner.width.saturating_sub(13) as usize)),
        ]),
        Line::from(vec![
            Span::styled("  worktree:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(truncate(&s.worktree_path, inner.width.saturating_sub(13) as usize)),
        ]),
        Line::from(vec![
            Span::styled("  branch:    ", Style::default().fg(Color::DarkGray)),
            Span::raw(&s.branch),
        ]),
    ];

    let info = Paragraph::new(lines);
    frame.render_widget(info, inner);
}

/// Middle block: scrollable PTY output.
fn render_pty_tail(frame: &mut Frame, area: Rect, sv: &SessionView) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " pty tail ",
            Style::default().fg(Color::DarkGray),
        ));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let visible = inner.height as usize;
    let total = sv.pty_tail.len();

    // Show last `visible` lines (auto-scroll to bottom)
    let skip = total.saturating_sub(visible);
    let lines: Vec<Line> = sv
        .pty_tail
        .iter()
        .skip(skip)
        .take(visible)
        .map(|l| Line::from(Span::raw(l.as_str())))
        .collect();

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

/// Bottom block: recent permission decisions.
fn render_decisions(frame: &mut Frame, area: Rect, sv: &SessionView) {
    if area.height < 2 {
        return;
    }

    let max_entries = (area.height as usize).saturating_sub(1); // 1 for header
    let start = sv.permissions.len().saturating_sub(max_entries);
    let entries = &sv.permissions[start..];

    let mut lines: Vec<Line> = Vec::with_capacity(entries.len() + 1);
    lines.push(Line::from(Span::styled(
        "recent decisions:",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::UNDERLINED),
    )));

    for entry in entries {
        let elapsed_secs = entry.timestamp.elapsed().as_secs();
        let time_str = if elapsed_secs < 60 {
            format!("{elapsed_secs}s ago")
        } else {
            format!("{}m ago", elapsed_secs / 60)
        };

        let decision_style = if entry.decision.starts_with('\u{2713}')
            || entry.decision.contains("allow")
            || entry.decision.contains("approved")
        {
            Style::default().fg(Color::Green)
        } else if entry.decision.contains("deny") || entry.decision.contains("denied") {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Yellow)
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<8}", time_str),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(truncate(&entry.request, 28)),
            Span::raw("  "),
            Span::styled(&entry.decision, decision_style),
            Span::raw(" "),
            Span::styled(
                format!("({})", entry.decided_by),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}

/// Truncate string to `max_len` chars, appending ellipsis if needed.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len > 3 {
        format!("{}...", &s[..max_len - 3])
    } else {
        s[..max_len].to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::PermissionEntry;
    use crate::session::{Session, SessionMode, SessionState};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::VecDeque;
    use std::time::Instant;

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf.cell((x, y)).unwrap().symbol());
            }
        }
        out
    }

    fn test_session_view() -> SessionView {
        let session = Session {
            id: "sess-001".to_string(),
            name: "explore-agents".to_string(),
            task: "investigate auth-service oauth flow".to_string(),
            worktree_path: "~/.config/orc/wt/explore-agents-a1b2/".to_string(),
            branch: "orc/explore-agents-a1b2".to_string(),
            base_commit: "abc1234".to_string(),
            tmux_session: "orc-explore-agents".to_string(),
            state: SessionState::Running,
            mode: SessionMode::Watch,
            model: "sonnet".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ended_at: None,
        };

        let mut pty_tail = VecDeque::new();
        pty_tail.push_back("> looking at auth-service/src/oauth/".to_string());
        pty_tail.push_back("> found callback.rs, token.rs, refresh.rs".to_string());

        let permissions = vec![
            PermissionEntry {
                timestamp: Instant::now(),
                request: "read in repo".to_string(),
                decision: "allow".to_string(),
                decided_by: "policy: allow.read".to_string(),
            },
            PermissionEntry {
                timestamp: Instant::now(),
                request: "bash: cargo test".to_string(),
                decision: "allow".to_string(),
                decided_by: "haiku, conf 0.92".to_string(),
            },
        ];

        SessionView {
            session,
            pty_tail,
            permissions,
            tab_index: 0,
        }
    }

    #[test]
    fn render_worker_does_not_panic() {
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let sv = test_session_view();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv);
            })
            .unwrap();
    }

    #[test]
    fn render_worker_tiny_area_no_panic() {
        let backend = TestBackend::new(20, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let sv = test_session_view();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv);
            })
            .unwrap();
    }

    #[test]
    fn render_worker_no_permissions() {
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut sv = test_session_view();
        sv.permissions.clear();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv);
            })
            .unwrap();
    }

    #[test]
    fn render_worker_empty_pty() {
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut sv = test_session_view();
        sv.pty_tail.clear();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv);
            })
            .unwrap();
    }

    #[test]
    fn render_worker_many_pty_lines() {
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut sv = test_session_view();
        sv.pty_tail.clear();
        for i in 0..200 {
            sv.pty_tail.push_back(format!("line {i}: some output text"));
        }

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv);
            })
            .unwrap();
    }

    #[test]
    fn render_worker_blocked_state() {
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut sv = test_session_view();
        sv.session.state = SessionState::Blocked {
            kind: crate::session::BlockKind::Permission,
            reason: "write /etc/passwd".to_string(),
        };

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv);
            })
            .unwrap();
    }

    #[test]
    fn render_worker_done_state() {
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut sv = test_session_view();
        sv.session.state = SessionState::Done {
            summary: "completed all tasks".to_string(),
        };

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv);
            })
            .unwrap();
    }

    #[test]
    fn render_worker_contains_session_name() {
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let sv = test_session_view();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv);
            })
            .unwrap();

        let content = buffer_text(&terminal);

        assert!(content.contains("explore-agents"), "buffer should contain session name");
        assert!(content.contains("sonnet"), "buffer should contain model");
        assert!(content.contains("orc/explore-agents-a1b2"), "buffer should contain branch");
    }

    #[test]
    fn render_worker_contains_pty_content() {
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let sv = test_session_view();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv);
            })
            .unwrap();

        let content = buffer_text(&terminal);

        assert!(
            content.contains("looking at auth-service"),
            "buffer should contain pty output"
        );
    }

    #[test]
    fn render_worker_contains_decisions() {
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let sv = test_session_view();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv);
            })
            .unwrap();

        let content = buffer_text(&terminal);

        assert!(
            content.contains("recent decisions"),
            "buffer should contain decisions header"
        );
        assert!(
            content.contains("read in repo"),
            "buffer should contain permission request"
        );
    }

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_exact_length() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long_string() {
        assert_eq!(truncate("hello world", 8), "hello...");
    }

    #[test]
    fn truncate_very_small_max() {
        assert_eq!(truncate("hello", 2), "he");
    }

    #[test]
    fn state_label_variants() {
        assert_eq!(state_label(&SessionState::Running), "Running");
        assert_eq!(
            state_label(&SessionState::Blocked {
                kind: crate::session::BlockKind::Permission,
                reason: String::new()
            }),
            "Blocked"
        );
        assert_eq!(
            state_label(&SessionState::AwaitingReview {
                diff_hash: String::new()
            }),
            "AwaitingReview"
        );
        assert_eq!(
            state_label(&SessionState::Done {
                summary: String::new()
            }),
            "Done"
        );
        assert_eq!(
            state_label(&SessionState::Failed {
                reason: String::new()
            }),
            "Failed"
        );
    }
}
