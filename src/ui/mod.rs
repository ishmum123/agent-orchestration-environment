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
fn autoscroll(app: &mut App, content_area: Rect, inner_w: u16, inner_h: usize) {
    let tabs: Vec<TabId> = std::iter::once(TabId::Orc)
        .chain((0..app.sessions.len()).map(TabId::Worker))
        .collect();

    for tab in tabs {
        // Worker tabs render a task banner and a decisions strip that
        // consume rows from content_area before the events block.  Match
        // render_worker's Layout so max_scroll here equals the one computed
        // inside render_event_log.
        let (effective_inner_h, wrapped) = match tab {
            TabId::Orc => {
                let wrapped = worker::wrapped_line_count(&app.orc_view.event_log, inner_w);
                (inner_h, wrapped)
            }
            TabId::Worker(i) => {
                let sv = &app.sessions[i];
                let task_is_empty = sv.session.task.trim().is_empty();
                let task_h: usize = if task_is_empty || content_area.height < 6 { 0 } else { 1 };
                let dec_count = sv.permissions.len().min(8);
                let dec_h: usize = if dec_count > 0 { dec_count + 2 } else { 0 };
                let effective = inner_h.saturating_sub(task_h + dec_h);
                let wrapped = worker::wrapped_line_count(&sv.event_log, inner_w);
                (effective, wrapped)
            }
        };
        let max = wrapped.saturating_sub(effective_inner_h);
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

/// Render the orc tab as an event-log peer to worker tabs. On a fresh
/// session (only system bootstrap lines, no user/assistant content yet),
/// show a welcome panel instead of two cryptic bracket lines.
fn render_orc_tab(frame: &mut Frame, area: Rect, app: &App) {
    use crate::app::LogEntry;
    let has_real_content = app
        .orc_view
        .event_log
        .iter()
        .any(|e| !matches!(e, LogEntry::System(_)));
    if !has_real_content {
        render_orc_welcome(frame, area, app);
        return;
    }
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

/// Centred welcome panel inside the events frame. Shown on first paint
/// when the orchestrator has not yet received any work.
fn render_orc_welcome(frame: &mut Frame, area: Rect, app: &App) {
    use ratatui::widgets::{Block, Borders};
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" events ", Style::default().fg(Color::DarkGray)));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 4 || inner.width < 20 {
        return;
    }

    let model = if app.orc_view.model.is_empty() {
        "opus".to_string()
    } else {
        app.orc_view.model.clone()
    };

    let lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  ◐ orc",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  · {model}"),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  the orchestrator is ready. give it a task and it will plan,",
            Style::default().fg(Color::Gray),
        )),
        Line::from(Span::styled(
            "  delegate to workers, and report back.",
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "c",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  chat with orc (give it a task)", Style::default()),
        ]),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "?",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  show all keys", Style::default()),
        ]),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "q",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  quit", Style::default()),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  workers spawn into their own git worktrees. you read their tabs;",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  you intervene with c, x, R, r — never with raw shell access.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("project: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                app.project_dir.display().to_string(),
                Style::default().fg(Color::Gray),
            ),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

/// One-line header. Left: focused entity (badge + name + state hint).
/// Right: project basename + worker tally (so multi-orc instances are
/// distinguishable at a glance).
fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    use crate::session::{BlockKind, SessionState};

    let mut left: Vec<Span<'static>> = match app.focused_tab {
        TabId::Orc => {
            let badge = if app.orc_view.alive { "◐" } else { "✗" };
            let badge_color = if app.orc_view.alive {
                Color::Cyan
            } else {
                Color::Red
            };
            vec![
                Span::raw(" "),
                Span::styled(
                    badge.to_string(),
                    Style::default()
                        .fg(badge_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled("orc", Style::default().add_modifier(Modifier::BOLD)),
            ]
        }
        TabId::Worker(idx) => {
            if let Some(sv) = app.sessions.get(idx) {
                let (badge, color) = App::state_badge(&sv.session.state);
                let state_hint = match &sv.session.state {
                    SessionState::Running => None,
                    SessionState::Blocked { kind, .. } => Some(match kind {
                        BlockKind::Permission => "blocked: permission",
                        BlockKind::OrcDecision => "blocked: orc",
                        BlockKind::UserInput => "blocked: question",
                    }),
                    SessionState::AwaitingReview { .. } => Some("awaiting review"),
                    SessionState::Done { .. } => Some("done"),
                    SessionState::Failed { .. } => Some("failed"),
                };
                let mut spans = vec![
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
                ];
                if let Some(hint) = state_hint {
                    let hint_style = match &sv.session.state {
                        SessionState::AwaitingReview { .. } => Style::default()
                            .fg(Color::Black)
                            .bg(color)
                            .add_modifier(Modifier::BOLD),
                        SessionState::Failed { .. } => Style::default()
                            .fg(Color::Black)
                            .bg(color)
                            .add_modifier(Modifier::BOLD),
                        _ => Style::default().fg(color),
                    };
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(format!(" {hint} "), hint_style));
                    if matches!(&sv.session.state, SessionState::AwaitingReview { .. }) {
                        spans.push(Span::styled(
                            "  press r",
                            Style::default().fg(Color::DarkGray),
                        ));
                    } else if matches!(&sv.session.state, SessionState::Failed { .. }) {
                        spans.push(Span::styled(
                            "  press R to restart",
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                }
                spans
            } else {
                vec![Span::raw("")]
            }
        }
    };

    // Right side: project basename · worker tally.
    let project_name = app
        .project_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let mut tally_parts: Vec<(String, Color)> = Vec::new();
    let n = app.sessions.len();
    if n > 0 {
        tally_parts.push((format!("{n} worker{}", if n == 1 { "" } else { "s" }), Color::DarkGray));
        let mut review = 0usize;
        let mut blocked = 0usize;
        let mut failed = 0usize;
        for sv in &app.sessions {
            match &sv.session.state {
                SessionState::AwaitingReview { .. } => review += 1,
                SessionState::Blocked { .. } => blocked += 1,
                SessionState::Failed { .. } => failed += 1,
                _ => {}
            }
        }
        if review > 0 {
            tally_parts.push((format!("◑ {review}"), Color::Magenta));
        }
        if blocked > 0 {
            tally_parts.push((format!("! {blocked}"), Color::Yellow));
        }
        if failed > 0 {
            tally_parts.push((format!("✗ {failed}"), Color::Red));
        }
    }

    let mut right: Vec<Span<'static>> = Vec::new();
    if !project_name.is_empty() {
        right.push(Span::styled(
            project_name,
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
    }
    for (i, (text, color)) in tally_parts.into_iter().enumerate() {
        if i == 0 && !right.is_empty() {
            right.push(Span::styled("  ", Style::default()));
        } else if i > 0 {
            right.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        }
        // First entry is plain "N workers" — keep it dim. Subsequent
        // attention-grabbing badges (review/blocked/failed) get bold so the
        // eye finds them immediately when there are many tabs.
        let style = if i == 0 {
            Style::default().fg(color)
        } else {
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        };
        right.push(Span::styled(text, style));
    }
    right.push(Span::raw(" "));

    let total_w = area.width as usize;
    let left_w: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right_w: usize = right.iter().map(|s| s.content.chars().count()).sum();
    let mut spans = std::mem::take(&mut left);
    if total_w > left_w + right_w {
        spans.push(Span::raw(" ".repeat(total_w - left_w - right_w)));
        spans.extend(right);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
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

    // In review mode the global keymap is replaced by a review keymap;
    // show only the review keys so the user doesn't see disabled hints.
    if app.review.is_some() {
        let review_parts: &[(&str, &str)] = &[
            ("a", "approve & merge"),
            ("c", "comment"),
            ("s", "submit with comments"),
            ("o", "open file"),
            ("e", "editor"),
            ("J/K", "next/prev hunk"),
            ("[ ]", "prev/next file"),
            ("j/k", "line up/down"),
            ("q/esc", "close review"),
        ];
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(review_parts.len() * 4 + 1);
        spans.push(Span::raw(" "));
        let key_style = Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD);
        let lbl_style = Style::default().fg(Color::DarkGray);
        let sep_style = Style::default().fg(Color::DarkGray);
        for (i, (k, lbl)) in review_parts.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled("   ", sep_style));
            }
            spans.push(Span::styled(k.to_string(), key_style));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(lbl.to_string(), lbl_style));
        }
        let bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Black));
        frame.render_widget(bar, area);
        return;
    }

    use crate::session::SessionState;

    // Build hints. `1-9 tab` only makes sense once at least one worker
    // exists. Worker-state-specific keys come first so the most relevant
    // action is closest to the eye.
    let mut parts: Vec<(&'static str, &'static str)> = Vec::new();
    parts.push(("c", "chat"));

    match app.focused_tab {
        TabId::Orc => {
            // If a worker is awaiting review, surface `r review` on the
            // orc tab so the user can act from there without first
            // hunting for the right worker tab.
            let any_review = app.sessions.iter().any(|sv| {
                matches!(sv.session.state, SessionState::AwaitingReview { .. })
            });
            if any_review {
                parts.push(("r", "review"));
            }
            parts.push(("^C", "interrupt"));
        }
        TabId::Worker(idx) => {
            if let Some(sv) = app.sessions.get(idx) {
                match &sv.session.state {
                    SessionState::AwaitingReview { .. } => {
                        parts.push(("r", "review"));
                        parts.push(("⏎", "control"));
                        parts.push(("x", "kill"));
                    }
                    SessionState::Failed { .. } => {
                        parts.push(("R", "restart"));
                        parts.push(("x", "kill"));
                    }
                    SessionState::Done { .. } => {
                        parts.push(("x", "kill"));
                    }
                    SessionState::Running | SessionState::Blocked { .. } => {
                        parts.push(("⏎", "control"));
                        parts.push(("^C", "interrupt"));
                        parts.push(("x", "kill"));
                    }
                }
            }
        }
    }

    if !app.sessions.is_empty() {
        parts.push(("Tab", "next"));
        if app.sessions.len() >= 1 {
            parts.push(("1-9", "jump"));
        }
        // Show `n` only when at least one worker needs attention —
        // otherwise the key would be a no-op and is just clutter.
        let any_attention = app.sessions.iter().any(|sv| {
            matches!(
                sv.session.state,
                SessionState::AwaitingReview { .. }
                    | SessionState::Failed { .. }
                    | SessionState::Blocked { .. }
            )
        });
        if any_attention {
            parts.push(("n", "next ⚡"));
        }
    }
    parts.push(("G", "end"));
    parts.push(("?", "help"));
    parts.push(("q", "quit"));

    // Render with subtle styling: keys in a brighter dim, descriptions
    // in a fainter dim so the eye groups (key, label) pairs.
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(parts.len() * 4 + 1);
    spans.push(Span::raw(" "));
    let key_style = Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD);
    let lbl_style = Style::default().fg(Color::DarkGray);
    let sep_style = Style::default().fg(Color::DarkGray);
    for (i, (k, lbl)) in parts.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("   ", sep_style));
        }
        spans.push(Span::styled(k.to_string(), key_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(lbl.to_string(), lbl_style));
    }

    let bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Black));
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
