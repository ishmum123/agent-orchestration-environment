// Top-level UI render dispatch.
//
// Layout:
//   Row 0: tab strip (2 lines)
//   Row 1: main content area (orc tab or worker tab — both render event_log)
//   Row 2: action bar (1 line)
//   Modal overlay if active

pub mod modals;
pub mod review;
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
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    tabs::render_tabs(frame, layout[0], app);

    if let Some(rev) = &app.review {
        review::render_review(frame, layout[1], rev);
    } else {
        match app.focused_tab {
            TabId::Orc => {
                render_orc_tab(frame, layout[1], app);
            }
            TabId::Worker(idx) => {
                if let Some(sv) = app.sessions.get(idx) {
                    worker::render_worker(
                        frame,
                        layout[1],
                        sv,
                        app.scroll_pos(TabId::Worker(idx)),
                    );
                }
            }
        }
    }

    render_action_bar(frame, layout[2], app);

    if let Some(modal) = &app.modal {
        modals::render_modal(frame, area, modal);
    }
}

/// Render the orc tab as an event-log peer to worker tabs.
fn render_orc_tab(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).split(area);

    let alive_str = if app.orc_view.alive { "alive" } else { "dead" };
    let header = Line::from(vec![
        Span::styled("orc", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" · "),
        Span::styled("brain", Style::default().fg(Color::Cyan)),
        Span::raw(" · "),
        Span::styled(alive_str, Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(header), chunks[0]);

    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " events ",
        Style::default().fg(Color::DarkGray),
    ));
    let inner = block.inner(chunks[1]);
    frame.render_widget(block, chunks[1]);
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
