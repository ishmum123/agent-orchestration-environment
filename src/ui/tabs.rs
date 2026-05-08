// Tab strip: switch between orc, worker, and review views.
//
// Renders a horizontal tab bar at the top of the terminal.
// Orc tab pinned left, worker tabs to the right.
// Each tab: state badge + name + elapsed time. Focused tab highlighted.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{App, TabId};
use crate::session::SessionMode;

/// Render the tab strip into the given area.
pub fn render_tabs(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focused_tab;

    let mut spans: Vec<Span> = Vec::new();

    // Orc tab — always first
    let orc_focused = focused == TabId::Orc;
    let orc_elapsed = {
        let secs = app.started_at.elapsed().as_secs();
        if secs < 60 {
            format!("{secs}s")
        } else {
            format!("{}m", secs / 60)
        }
    };

    let orc_badge = if app.orc_view.alive { "◐" } else { "✗" };
    let orc_label = format!(" {orc_badge} ORC {orc_elapsed} ");
    if orc_focused {
        spans.push(Span::styled(
            orc_label,
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::styled(
            orc_label,
            Style::default().fg(Color::Cyan),
        ));
    }

    // Worker tabs
    for (idx, sv) in app.sessions.iter().enumerate() {
        let is_focused = focused == TabId::Worker(idx);
        let (badge, badge_color) = App::state_badge(&sv.session.state);
        let elapsed = App::elapsed_str(&sv.session);
        let name = &sv.session.name;

        let label = if is_focused {
            let mode_tag = match sv.session.mode {
                SessionMode::Watch => "[WATCH]",
                SessionMode::Control => "[CTRL]",
            };
            format!(" {badge} {name} {elapsed} {mode_tag} ")
        } else {
            format!(" {badge} {name} {elapsed} ")
        };

        spans.push(Span::raw("│"));

        if is_focused {
            spans.push(Span::styled(
                label,
                Style::default()
                    .fg(Color::Black)
                    .bg(badge_color)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                label,
                Style::default().fg(badge_color),
            ));
        }
    }

    let line = Line::from(spans);
    let block = Block::default().borders(Borders::BOTTOM);
    let paragraph = Paragraph::new(line).block(block);
    frame.render_widget(paragraph, area);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::session::{BlockKind, Session, SessionMode, SessionState};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn test_session(id: &str, name: &str, state: SessionState) -> Session {
        Session {
            id: id.to_string(),
            name: name.to_string(),
            task: "test".to_string(),
            worktree_path: "/tmp/wt".to_string(),
            branch: "test".to_string(),
            base_commit: "abc123".to_string(),
            tmux_session: format!("orc-{name}"),
            state,
            mode: SessionMode::Watch,
            model: "sonnet".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ended_at: None,
            claude_session_id: None,
        }
    }

    fn render_to_strings(app: &App, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, width, height);
                render_tabs(frame, area, app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut lines = Vec::new();
        for y in 0..height {
            let mut line = String::new();
            for x in 0..width {
                line.push_str(buffer.cell((x, y)).unwrap().symbol());
            }
            lines.push(line);
        }
        lines
    }

    #[test]
    fn orc_tab_only() {
        let app = App::new(".");
        let lines = render_to_strings(&app, 40, 2);
        // First line should contain ORC
        assert!(lines[0].contains("ORC"), "expected ORC in: {:?}", lines[0]);
        assert!(lines[0].contains("◐"), "expected ◐ in: {:?}", lines[0]);
    }

    #[test]
    fn worker_tabs_appear() {
        let mut app = App::new(".");
        app.add_session(test_session("s1", "explore", SessionState::Running));
        app.add_session(test_session(
            "s2",
            "migrate",
            SessionState::AwaitingReview {
                diff_hash: String::new(),
            },
        ));
        app.add_session(test_session(
            "s3",
            "check",
            SessionState::Done {
                summary: String::new(),
            },
        ));

        let lines = render_to_strings(&app, 80, 2);
        let top = &lines[0];
        assert!(top.contains("ORC"), "expected ORC in: {top:?}");
        assert!(top.contains("explore"), "expected explore in: {top:?}");
        assert!(top.contains("migrate"), "expected migrate in: {top:?}");
        assert!(top.contains("check"), "expected check in: {top:?}");
        // Badge characters
        assert!(top.contains("◐"), "expected ◐ running badge in: {top:?}");
        assert!(top.contains("◑"), "expected ◑ review badge in: {top:?}");
        assert!(top.contains("✓"), "expected ✓ done badge in: {top:?}");
    }

    #[test]
    fn focused_worker_shows_mode() {
        let mut app = App::new(".");
        let mut s = test_session("s1", "worker1", SessionState::Running);
        s.mode = SessionMode::Control;
        app.add_session(s);
        app.focus_tab(TabId::Worker(0));

        let lines = render_to_strings(&app, 60, 2);
        let top = &lines[0];
        assert!(
            top.contains("[CTRL]"),
            "expected [CTRL] for focused control-mode tab in: {top:?}"
        );
    }

    #[test]
    fn focused_worker_watch_mode() {
        let mut app = App::new(".");
        app.add_session(test_session("s1", "worker1", SessionState::Running));
        app.focus_tab(TabId::Worker(0));

        let lines = render_to_strings(&app, 60, 2);
        let top = &lines[0];
        assert!(
            top.contains("[WATCH]"),
            "expected [WATCH] for focused watch-mode tab in: {top:?}"
        );
    }

    #[test]
    fn unfocused_tab_no_mode_tag() {
        let mut app = App::new(".");
        app.add_session(test_session("s1", "worker1", SessionState::Running));
        // Orc is focused, worker is not
        app.focus_tab(TabId::Orc);

        let lines = render_to_strings(&app, 60, 2);
        let top = &lines[0];
        assert!(
            !top.contains("[WATCH]") && !top.contains("[CTRL]"),
            "unfocused tab should not show mode tag in: {top:?}"
        );
    }

    #[test]
    fn blocked_states_badges() {
        let mut app = App::new(".");
        app.add_session(test_session(
            "s1",
            "perm",
            SessionState::Blocked {
                kind: BlockKind::Permission,
                reason: String::new(),
            },
        ));
        app.add_session(test_session(
            "s2",
            "orc",
            SessionState::Blocked {
                kind: BlockKind::OrcDecision,
                reason: String::new(),
            },
        ));
        app.add_session(test_session(
            "s3",
            "user",
            SessionState::Blocked {
                kind: BlockKind::UserInput,
                reason: String::new(),
            },
        ));
        app.add_session(test_session(
            "s4",
            "fail",
            SessionState::Failed {
                reason: String::new(),
            },
        ));

        let lines = render_to_strings(&app, 80, 2);
        let top = &lines[0];
        assert!(top.contains("?"), "expected ? permission badge in: {top:?}");
        assert!(top.contains("⏸"), "expected ⏸ orc-decision badge in: {top:?}");
        assert!(top.contains("!"), "expected ! user-input badge in: {top:?}");
        assert!(top.contains("✗"), "expected ✗ failed badge in: {top:?}");
    }

    #[test]
    fn separator_between_tabs() {
        let mut app = App::new(".");
        app.add_session(test_session("s1", "w1", SessionState::Running));

        let lines = render_to_strings(&app, 40, 2);
        let top = &lines[0];
        assert!(top.contains("│"), "expected │ separator in: {top:?}");
    }
}
