// Top-level UI render dispatch.
//
// Layout:
//   Row 0: main area split horizontally into [content | agents panel]
//   Row 1: action bar (1 line)
//   Modal overlay if active
//
// The top tab strip was removed in the UX overhaul; the agents panel
// (right-hand side) doubles as the visual tab list.

pub mod modals;
pub mod panel;
pub mod review;
#[allow(dead_code)]
pub mod tabs;
pub mod worker;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::prelude::*;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, TabId};

/// Render the full UI frame.
pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // main
            Constraint::Length(1), // action bar
        ])
        .split(area);

    render_header(frame, layout[0], app);

    // Split main area into [content | agents panel].
    let panel_w = panel::PANEL_WIDTH.min(layout[1].width.saturating_sub(20));
    let main_split = Layout::horizontal([
        Constraint::Min(20),
        Constraint::Length(panel_w),
    ])
    .split(layout[1]);
    let content_area = main_split[0];
    let panel_area = main_split[1];

    if let Some(rev) = &app.review {
        review::render_review(frame, content_area, rev);
    } else {
        match app.focused_tab {
            TabId::Orc => {
                render_orc_tab(frame, content_area, app);
            }
            TabId::Worker(idx) => {
                if let Some(sv) = app.sessions.get(idx) {
                    worker::render_worker(
                        frame,
                        content_area,
                        sv,
                        app.scroll_pos(TabId::Worker(idx)),
                    );
                }
            }
        }
    }

    panel::render_panel(frame, panel_area, app);

    render_action_bar(frame, layout[2], app);

    if let Some(modal) = &app.modal {
        modals::render_modal(frame, area, modal);
    }
}

/// Render the orc tab as an event-log peer to worker tabs.
fn render_orc_tab(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " events ",
        Style::default().fg(Color::DarkGray),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let lines = worker::log_lines(&app.orc_view.event_log);
    let scroll = app.scroll_pos(TabId::Orc);
    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));
    frame.render_widget(para, inner);
}

/// Minimal one-line header showing the focused entity.
fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let line = match app.focused_tab {
        TabId::Orc => {
            let badge = if app.orc_view.alive { "◐" } else { "✗" };
            let badge_color = if app.orc_view.alive {
                Color::Cyan
            } else {
                Color::Red
            };
            Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    badge.to_string(),
                    Style::default()
                        .fg(badge_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled("orc", Style::default().add_modifier(Modifier::BOLD)),
            ])
        }
        TabId::Worker(idx) => {
            if let Some(sv) = app.sessions.get(idx) {
                let (badge, color) = App::state_badge(&sv.session.state);
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        badge.to_string(),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        sv.session.name.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ])
            } else {
                Line::from("")
            }
        }
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// Bottom action bar — context-sensitive keybinds.
fn render_action_bar(frame: &mut Frame, area: Rect, app: &App) {
    let hints = match (&app.focused_tab, &app.modal) {
        (_, Some(_)) => return,
        (TabId::Orc, None) => "n speak   1-9 tab   ^C interrupt   q quit   ? help",
        (TabId::Worker(idx), None) => {
            if let Some(sv) = app.sessions.get(*idx) {
                match &sv.session.state {
                    crate::session::SessionState::AwaitingReview { .. } => {
                        "n speak   r review   c control   k kill   ^C interrupt   q quit   ? help"
                    }
                    crate::session::SessionState::Failed { .. } => {
                        "R restart   k kill   q quit   ? help"
                    }
                    _ => "n speak   c control   k kill   ^C interrupt   q quit   ? help",
                }
            } else {
                "q quit   ? help"
            }
        }
    };

    let bar = Paragraph::new(Line::from(vec![
        Span::styled(" ", Style::default()),
        Span::styled(hints.to_string(), Style::default().fg(Color::DarkGray)),
    ]))
    .style(Style::default().bg(Color::Black));

    frame.render_widget(bar, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Modal;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn render_does_not_panic_empty_app() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::new(".");
        terminal.draw(|f| render(f, &app)).unwrap();
    }

    #[test]
    fn render_does_not_panic_with_sessions() {
        use crate::session::{Session, SessionMode, SessionState};

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(".");

        app.add_session(Session {
            id: "s1".to_string(),
            name: "worker1".to_string(),
            task: "test".to_string(),
            worktree_path: "/tmp".to_string(),
            branch: "main".to_string(),
            base_commit: "abc".to_string(),
            tmux_session: "orc-w1".to_string(),
            state: SessionState::Running,
            mode: SessionMode::Watch,
            model: "sonnet".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ended_at: None,
            claude_session_id: None,
        });

        terminal.draw(|f| render(f, &app)).unwrap();
        app.focus_tab(TabId::Worker(0));
        terminal.draw(|f| render(f, &app)).unwrap();
    }

    #[test]
    fn render_with_modal() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(".");
        app.modal = Some(Modal::Help);
        terminal.draw(|f| render(f, &app)).unwrap();
    }
}
