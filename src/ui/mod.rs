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
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{App, TabId};

/// Render the full UI frame. Takes `&mut App` so autoscroll can pin the
/// scroll position to the tail when `stick_to_bottom` is set.
pub fn render(frame: &mut Frame, app: &mut App) {
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

    // Autoscroll pass: if a tab is sticky-to-bottom, pin its scroll value
    // to the end before rendering. Width used for wrap accounting matches
    // the inner width of the events box.
    let events_inner_w = content_area.width.saturating_sub(2);
    let events_inner_h = content_area.height.saturating_sub(2) as usize;
    autoscroll(app, content_area, events_inner_w, events_inner_h);

    // Hard-clear the chat and panel regions before drawing — works
    // around stale-cell bleed between tab switches with Paragraph wrap.
    frame.render_widget(Clear, content_area);
    frame.render_widget(Clear, panel_area);

    if let Some(rev) = &app.review {
        review::render_review(frame, content_area, rev);
    } else {
        match app.focused_tab {
            TabId::Orc => {
                render_orc_tab(frame, content_area, app);
            }
            TabId::Worker(idx) => {
                let scroll = app.scroll_pos(TabId::Worker(idx));
                let tick = app.tick;
                if let Some(sv) = app.sessions.get(idx) {
                    worker::render_worker(frame, content_area, sv, scroll, tick);
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

/// Pin scroll. If a tab is sticky-to-bottom, set scroll to the tail.
/// If not sticky but the user has scrolled down to the bottom, re-engage
/// stick (so subsequent new content keeps following).
fn autoscroll(app: &mut App, _content_area: Rect, inner_w: u16, inner_h: usize) {
    let tabs: Vec<TabId> = std::iter::once(TabId::Orc)
        .chain((0..app.sessions.len()).map(TabId::Worker))
        .collect();

    for tab in tabs {
        let log_ref: &[crate::app::LogEntry] = match tab {
            TabId::Orc => &app.orc_view.event_log,
            TabId::Worker(i) => &app.sessions[i].event_log,
        };
        let wrapped = worker::wrapped_line_count(log_ref, inner_w);
        let max = wrapped.saturating_sub(inner_h);
        let stick = app.stick_to_bottom(tab);
        let cur = app.scroll_pos(tab);

        if stick {
            app.set_scroll(tab, max);
        } else if cur >= max {
            // User scrolled down to (or past) the bottom — re-engage follow.
            app.set_stick(tab, true);
            app.set_scroll(tab, max);
        }
    }
}

/// Render the orc tab as an event-log peer to worker tabs.
fn render_orc_tab(frame: &mut Frame, area: Rect, app: &App) {
    let scroll = app.scroll_pos(TabId::Orc);
    worker::render_event_log(
        frame,
        area,
        &app.orc_view.event_log,
        scroll,
        app.orc_view.is_thinking,
        app.tick,
    );
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
    // While ask_user modal is hidden (peeking chat), show a clear hint.
    if let Some(crate::app::Modal::AskUser { hidden: true, .. }) = &app.modal {
        let bar = Paragraph::new(Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(
                "pending question — Tab resume   j/k Up/Down scroll   G end",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(Color::Black));
        frame.render_widget(bar, area);
        return;
    }
    if app.modal.is_some() {
        return;
    }

    use crate::session::SessionState;

    let mut parts: Vec<&'static str> = Vec::new();
    parts.push("t talk");
    parts.push("1-9 tab");

    match app.focused_tab {
        TabId::Orc => {
            parts.push("^C interrupt");
            parts.push("G end");
        }
        TabId::Worker(idx) => {
            if let Some(sv) = app.sessions.get(idx) {
                match &sv.session.state {
                    SessionState::AwaitingReview { .. } => {
                        parts.push("r review");
                        parts.push("c control");
                        parts.push("k kill");
                        parts.push("G end");
                    }
                    SessionState::Failed { .. } => {
                        parts.push("R restart");
                        parts.push("k kill");
                        parts.push("G end");
                    }
                    SessionState::Done { .. } => {
                        parts.push("k kill");
                        parts.push("G end");
                    }
                    SessionState::Running | SessionState::Blocked { .. } => {
                        parts.push("c control");
                        parts.push("k kill");
                        parts.push("^C interrupt");
                        parts.push("G end");
                    }
                }
            }
        }
    }

    parts.push("q quit");
    parts.push("? help");

    let hints = parts.join("   ");

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
    use crate::app::Modal;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn render_does_not_panic_empty_app() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(".");
        terminal.draw(|f| render(f, &mut app)).unwrap();
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

        terminal.draw(|f| render(f, &mut app)).unwrap();
        app.focus_tab(TabId::Worker(0));
        terminal.draw(|f| render(f, &mut app)).unwrap();
    }

    #[test]
    fn render_with_modal() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(".");
        app.modal = Some(Modal::Help);
        terminal.draw(|f| render(f, &mut app)).unwrap();
    }
}
