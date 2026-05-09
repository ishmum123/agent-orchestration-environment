// Worker tab: structured event-log view with session info and permission log.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, LogEntry, SessionView};
use crate::session::SessionState;

/// Render the worker tab for a single session.
///
/// Header is rendered by `ui::render_header` above this area, so we only
/// need event log + (optional) decisions strip.
pub fn render_worker(frame: &mut Frame, area: Rect, session_view: &SessionView, scroll: usize) {
    let decision_count = session_view.permissions.len().min(8);
    let decisions_height = if decision_count > 0 {
        (decision_count + 2) as u16
    } else {
        0
    };

    let chunks = Layout::vertical([
        Constraint::Min(4),
        Constraint::Length(decisions_height),
    ])
    .split(area);

    render_event_log(frame, chunks[0], &session_view.event_log, scroll);
    if decisions_height > 0 {
        render_decisions(frame, chunks[1], session_view);
    }
}

#[allow(dead_code)]
fn state_label(state: &SessionState) -> &'static str {
    match state {
        SessionState::Running => "Running",
        SessionState::Blocked { .. } => "Blocked",
        SessionState::AwaitingReview { .. } => "AwaitingReview",
        SessionState::Done { .. } => "Done",
        SessionState::Failed { .. } => "Failed",
    }
}

#[allow(dead_code)]
fn render_session_info(
    frame: &mut Frame,
    area: Rect,
    sv: &SessionView,
    badge: &str,
    badge_color: Color,
    elapsed: &str,
) {
    let s = &sv.session;
    let title = Line::from(vec![
        Span::styled(s.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" \u{b7} "),
        Span::styled(s.model.clone(), Style::default().fg(Color::Cyan)),
    ]);
    let title_right = Line::from(vec![
        Span::styled(
            badge.to_string(),
            Style::default().fg(badge_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(state_label(&s.state), Style::default().fg(badge_color)),
        Span::raw("  "),
        Span::styled(elapsed.to_string(), Style::default().fg(Color::DarkGray)),
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

    let badge_width = title_right.width() as u16;
    if badge_width < inner.width {
        let badge_area = Rect {
            x: inner.x + inner.width - badge_width,
            y: area.y,
            width: badge_width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(title_right), badge_area);
    }

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
            Span::raw(s.branch.clone()),
        ]),
    ];

    let info = Paragraph::new(lines);
    frame.render_widget(info, inner);
}

/// Convert a structured log into renderable lines. Used by both the orc tab
/// and worker tabs — same model for both.
pub fn log_lines(log: &[LogEntry]) -> Vec<Line<'static>> {
    log.iter()
        .map(|entry| match entry {
            LogEntry::UserText(t) => Line::from(vec![
                Span::styled("you  ", Style::default().fg(Color::Cyan)),
                Span::raw(t.clone()),
            ]),
            LogEntry::AssistantText(t) => Line::from(Span::raw(t.clone())),
            LogEntry::Thinking(t) => Line::from(Span::styled(
                format!("(thinking) {t}"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )),
            LogEntry::ToolUse {
                name,
                input_summary,
            } => Line::from(vec![
                Span::styled("→ ", Style::default().fg(Color::Yellow)),
                Span::raw(format!("{name}({input_summary})")),
            ]),
            LogEntry::ToolResult { text, is_error } => {
                let color = if *is_error { Color::Red } else { Color::Green };
                let snippet = if text.len() > 200 {
                    format!("{}…", &text[..200])
                } else {
                    text.clone()
                };
                Line::from(vec![
                    Span::styled("← ", Style::default().fg(color)),
                    Span::raw(snippet),
                ])
            }
            LogEntry::System(s) => Line::from(Span::styled(
                format!("[{s}]"),
                Style::default().fg(Color::DarkGray),
            )),
            LogEntry::TurnEnd { cost_usd } => Line::from(Span::styled(
                cost_usd
                    .map(|c| format!("— turn end (${c:.4}) —"))
                    .unwrap_or_else(|| "— turn end —".into()),
                Style::default().fg(Color::DarkGray),
            )),
        })
        .collect()
}

fn render_event_log(frame: &mut Frame, area: Rect, log: &[LogEntry], scroll: usize) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " events ",
        Style::default().fg(Color::DarkGray),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let lines = log_lines(log);
    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));
    frame.render_widget(para, inner);
}

fn render_decisions(frame: &mut Frame, area: Rect, sv: &SessionView) {
    if area.height < 2 {
        return;
    }

    let max_entries = (area.height as usize).saturating_sub(1);
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
            Span::styled(entry.decision.clone(), decision_style),
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
            claude_session_id: None,
        };

        let event_log = vec![
            LogEntry::AssistantText("looking at auth-service/src/oauth/".into()),
            LogEntry::ToolUse {
                name: "Read".into(),
                input_summary: "callback.rs".into(),
            },
            LogEntry::ToolResult {
                text: "ok".into(),
                is_error: false,
            },
        ];

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
            event_log,
            permissions,
            tab_index: 0,
            claude_session_id: None,
            skip_next_tool_result: false,
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
                render_worker(frame, area, &sv, 0);
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
                render_worker(frame, area, &sv, 0);
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
                render_worker(frame, area, &sv, 0);
            })
            .unwrap();
    }

    #[test]
    fn render_worker_empty_event_log() {
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut sv = test_session_view();
        sv.event_log.clear();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv, 0);
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
                render_worker(frame, area, &sv, 0);
            })
            .unwrap();
        let content = buffer_text(&terminal);
        // Worker tab no longer renders its own session-info header — the
        // header is now rendered by ui::render_header above this area, and
        // session name appears in the agents panel. Keep the smoke test
        // by checking event log content.
        assert!(content.contains("looking at auth-service"), "{}", content);
    }

    #[test]
    fn render_worker_contains_decisions() {
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let sv = test_session_view();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv, 0);
            })
            .unwrap();
        let content = buffer_text(&terminal);
        assert!(content.contains("recent decisions"));
    }

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string() {
        assert_eq!(truncate("hello world", 8), "hello...");
    }

    #[test]
    fn state_label_variants() {
        assert_eq!(state_label(&SessionState::Running), "Running");
    }
}
