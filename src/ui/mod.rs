// Top-level UI render dispatch.
//
// Layout:
//   Row 0: tab strip (2 lines)
//   Row 1: main content area (dashboard or worker tab)
//   Row 2: action bar (1 line)
//   Modal overlay if active

pub mod dashboard;
pub mod modals;
pub mod review;
pub mod tabs;
pub mod worker;

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::{App, Modal, TabId};

/// Render the full UI frame.
pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // 3-row layout: tabs | content | action bar
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),  // tab strip
            Constraint::Min(1),    // main content
            Constraint::Length(1), // action bar
        ])
        .split(area);

    // Tab strip
    tabs::render_tabs(frame, layout[0], app);

    // Main content: review view takes over when active, otherwise normal tab content
    if let Some(rev) = &app.review {
        review::render_review(frame, layout[1], rev);
    } else {
        match app.focused_tab {
            TabId::Orc => {
                dashboard::render_dashboard(frame, layout[1], app);
            }
            TabId::Worker(idx) => {
                if let Some(sv) = app.sessions.get(idx) {
                    worker::render_worker(frame, layout[1], sv);
                }
            }
        }
    }

    // Action bar
    render_action_bar(frame, layout[2], app);

    // Modal overlay (rendered last, on top)
    if let Some(modal) = &app.modal {
        modals::render_modal(frame, area, modal);
    }
}

/// Bottom action bar — context-sensitive keybinds.
fn render_action_bar(frame: &mut Frame, area: Rect, app: &App) {
    let hints = match (&app.focused_tab, &app.modal) {
        (_, Some(_)) => return, // modal has its own hints
        (TabId::Orc, None) => {
            "n new   1-9 tab   g graph   q quit   ? help"
        }
        (TabId::Worker(idx), None) => {
            if let Some(sv) = app.sessions.get(*idx) {
                match &sv.session.state {
                    crate::session::SessionState::AwaitingReview { .. } => {
                        // Can't return a reference to a local, so use a static
                        "enter attach   r review   c control   k kill   q quit   ? help"
                    }
                    crate::session::SessionState::Failed { .. } => {
                        "R restart   k kill   q quit   ? help"
                    }
                    _ => {
                        "enter attach   c control   k kill   q quit   ? help"
                    }
                }
            } else {
                "q quit   ? help"
            }
        }
    };

    let bar = Paragraph::new(Line::from(vec![
        Span::styled(" ", Style::default()),
        Span::styled(hints, Style::default().fg(Color::DarkGray)),
    ]))
    .style(Style::default().bg(Color::Black));

    frame.render_widget(bar, area);
}

#[cfg(test)]
mod tests {
    use super::*;
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
        });

        // Render orc tab
        terminal.draw(|f| render(f, &app)).unwrap();

        // Render worker tab
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

    #[test]
    fn action_bar_changes_by_context() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::new(".");
        terminal.draw(|f| render(f, &app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        // Last line should have action hints
        let last_line: String = (0..buf.area.width)
            .map(|x| buf.cell((x, buf.area.height - 1)).unwrap().symbol().to_string())
            .collect();
        assert!(last_line.contains("graph"), "orc tab should show graph hint");
    }
}
