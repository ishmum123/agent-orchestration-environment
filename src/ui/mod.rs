// Top-level UI render dispatch.
//
// Layout:
//   Row 0: main area split horizontally into [content | agents panel]
//   Row 1: action bar (1 line)
//   Modal overlay if active
//
// The top tab strip was removed in the UX overhaul; the agents panel
// (right-hand side) doubles as the visual tab list.

pub mod backchannel;
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

    // Right-side agents panel is hidden when there are no workers — the
    // single orc card it would show is redundant with the welcome panel
    // and the header. Workers spawning reclaim the panel.
    let show_panel = !app.sessions.is_empty();
    let (content_area, panel_area) = if show_panel {
        let panel_w = panel::PANEL_WIDTH.min(layout[1].width.saturating_sub(20));
        let split = Layout::horizontal([
            Constraint::Min(20),
            Constraint::Length(panel_w),
        ])
        .split(layout[1]);
        (split[0], split[1])
    } else {
        (layout[1], Rect::new(0, 0, 0, 0))
    };

    // Autoscroll pass: if a tab is sticky-to-bottom, pin its scroll value
    // to the end before rendering. Width used for wrap accounting matches
    // the inner width of the events box.
    let events_inner_w = content_area.width.saturating_sub(2);
    let events_inner_h = content_area.height.saturating_sub(2) as usize;
    autoscroll(app, content_area, events_inner_w, events_inner_h);

    // Hard-clear the chat and panel regions before drawing — works
    // around stale-cell bleed between tab switches with Paragraph wrap.
    frame.render_widget(Clear, content_area);
    if show_panel {
        frame.render_widget(Clear, panel_area);
    }

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
                    let cbox = compose_box_height(&sv.compose.buffer);
                    let (top, bottom) = if content_area.height > cbox + 2 {
                        let split = Layout::vertical([
                            Constraint::Min(1),
                            Constraint::Length(cbox),
                        ])
                        .split(content_area);
                        (split[0], split[1])
                    } else {
                        (content_area, Rect::new(0, 0, 0, 0))
                    };
                    worker::render_worker(frame, top, sv, scroll, tick);
                    if bottom.height > 0 {
                        let title = format!("talk to {}", sv.session.name);
                        render_compose(
                            frame,
                            bottom,
                            &sv.compose,
                            &title,
                            sv.is_thinking,
                            tick,
                        );
                    }
                }
            }
        }
    }

    if show_panel {
        panel::render_panel(frame, panel_area, app);
    }

    render_action_bar(frame, layout[2], app);

    if let Some(modal) = &app.modal {
        modals::render_modal(frame, area, modal);
    }

    if let Some(bc) = app.backchannel.as_mut() {
        if bc.open {
            backchannel::render_overlay(frame, area, bc, app.tick);
        }
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
                // Orc tab reserves a variable number of rows at the bottom
                // for the compose box (grows with the buffer, capped). Match
                // that here so max_scroll lines up.
                let cbox = compose_box_height(&app.orc_view.compose.buffer);
                let compose_h = if content_area.height > cbox + 2 {
                    cbox as usize
                } else {
                    0
                };
                let effective = inner_h.saturating_sub(compose_h);
                let wrapped = worker::wrapped_line_count(&app.orc_view.event_log, inner_w, "orc");
                (effective, wrapped)
            }
            TabId::Worker(i) => {
                let sv = &app.sessions[i];
                let task_is_empty = sv.session.task.trim().is_empty();
                let task_h: usize = if task_is_empty || content_area.height < 6 { 0 } else { 1 };
                let dec_count = sv.permissions.len().min(8);
                let dec_h: usize = if dec_count > 0 { dec_count + 2 } else { 0 };
                let cbox = compose_box_height(&sv.compose.buffer);
                let compose_h: usize = if content_area.height > cbox + 2 {
                    cbox as usize
                } else {
                    0
                };
                let effective = inner_h.saturating_sub(task_h + dec_h + compose_h);
                let wrapped = worker::wrapped_line_count(&sv.event_log, inner_w, &sv.session.name);
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

/// Maximum number of text rows inside the compose box. The box itself
/// is `inner_rows + 2` tall (top + bottom border). Beyond this cap the
/// buffer is windowed to the tail so the user always sees what they're
/// currently typing.
const COMPOSE_MAX_INNER: usize = 6;

/// Compute the inner text-row count for the compose box given the
/// current buffer. Always at least 1 (so the placeholder fits when
/// the buffer is empty).
fn compose_inner_rows(buffer: &str) -> usize {
    if buffer.is_empty() {
        return 1;
    }
    buffer.split('\n').count().clamp(1, COMPOSE_MAX_INNER)
}

/// Total height of the compose box (inner rows + 2 for borders).
fn compose_box_height(buffer: &str) -> u16 {
    (compose_inner_rows(buffer) + 2) as u16
}

/// Render the orc tab as a chat-first surface: transcript (or welcome
/// panel when empty) on top, persistent input bar pinned to the bottom.
fn render_orc_tab(frame: &mut Frame, area: Rect, app: &App) {
    use crate::app::LogEntry;

    let compose_h = compose_box_height(&app.orc_view.compose.buffer);
    let (transcript_area, compose_area) = if area.height > compose_h + 2 {
        let split = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(compose_h),
        ])
        .split(area);
        (split[0], split[1])
    } else {
        (area, Rect::new(0, 0, 0, 0))
    };

    let has_real_content = app
        .orc_view
        .event_log
        .iter()
        .any(|e| !matches!(e, LogEntry::System(_)));
    if !has_real_content {
        render_orc_welcome(frame, transcript_area, app);
    } else {
        let scroll = app.scroll_pos(TabId::Orc);
        worker::render_event_log(
            frame,
            transcript_area,
            &app.orc_view.event_log,
            scroll,
            app.orc_view.is_thinking,
            app.tick,
            "orc",
        );
    }

    if compose_area.height > 0 {
        render_compose(
            frame,
            compose_area,
            &app.orc_view.compose,
            "talk to orc",
            app.orc_view.is_thinking,
            app.tick,
        );
    }
}

/// Persistent inline input bar for talking to an agent. Used by both
/// the orc tab (with `OrcView::compose`) and worker tabs (with
/// `SessionView::compose`). The title prefix labels who you're talking
/// to; `thinking` toggles the "◐ thinking" badge.
pub(crate) fn render_compose(
    frame: &mut Frame,
    area: Rect,
    cs: &crate::app::ComposeState,
    title_prefix: &str,
    thinking: bool,
    tick: u64,
) {
    use ratatui::widgets::{Block, Borders};
    let nav = cs.nav_mode;
    let title = if nav {
        " navigation mode — Esc to resume typing ".to_string()
    } else if thinking {
        format!(" {title_prefix} · ◐ thinking ")
    } else {
        format!(" {title_prefix} ")
    };
    let title_style = if nav {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else if thinking {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let border_style = if nav {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(title, title_style));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width < 4 {
        return;
    }

    let buffer = &cs.buffer;
    let attach_n = cs.attachments.len();
    let is_empty = buffer.is_empty() && attach_n == 0;

    if is_empty {
        let placeholder = if nav {
            "(typing disabled · Esc to resume)"
        } else {
            "type to talk · ⏎ send · Esc for shortcuts"
        };
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(Span::raw(" "));
        if attach_n > 0 {
            spans.push(Span::styled(
                format!("[{attach_n} img] "),
                Style::default().fg(Color::Magenta),
            ));
        }
        spans.push(Span::styled(
            placeholder.to_string(),
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), inner);
        return;
    }

    // Resolve cursor → (logical_row, col_chars).
    let cursor_byte = cs.cursor.min(buffer.len());
    let cursor_row = buffer[..cursor_byte].matches('\n').count();
    let line_start = buffer[..cursor_byte].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let cursor_col = buffer[line_start..cursor_byte].chars().count();

    // Window the buffer to a slice that keeps the cursor row visible.
    let all_lines: Vec<&str> = buffer.split('\n').collect();
    let total = all_lines.len();
    let visible_n = (inner.height as usize).min(total);
    // Default to showing the tail; bump the window up if the cursor is
    // somewhere earlier so it never scrolls off.
    let mut start = total.saturating_sub(visible_n);
    if cursor_row < start {
        start = cursor_row;
    }
    if cursor_row >= start + visible_n {
        start = (cursor_row + 1).saturating_sub(visible_n);
    }
    let visible = &all_lines[start..start + visible_n];

    let cursor_glyph = if (tick / 60) % 2 == 0 { "▏" } else { " " };

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(visible.len());
    for (i, line_text) in visible.iter().enumerate() {
        let abs_row = start + i;
        let is_cursor_row = abs_row == cursor_row;
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(Span::raw(" "));
        let mut prefix_w = 1usize;
        if i == 0 && start == 0 && attach_n > 0 {
            let chip = format!("[{attach_n} img] ");
            prefix_w += chip.chars().count();
            spans.push(Span::styled(
                chip,
                Style::default().fg(Color::Magenta),
            ));
        }

        let avail = (inner.width as usize).saturating_sub(prefix_w);

        if !is_cursor_row {
            // Non-cursor row: truncate the tail if too wide.
            let display: String = line_text.chars().take(avail).collect();
            spans.push(Span::raw(display));
            lines.push(Line::from(spans));
            continue;
        }

        // Cursor row: render the char under the cursor as a reverse-
        // video overlay so the cursor occupies the same cell as the
        // character. Blink by toggling the reverse modifier on/off.
        let line_chars: Vec<char> = line_text.chars().collect();
        let line_len = line_chars.len();
        // The cursor may sit one position past the last char (at line
        // end). Reserve space for that "virtual" cell.
        let virtual_len = line_len + 1;
        // Slide the visible window so the cursor cell stays in view.
        let h_start = if cursor_col < avail {
            0
        } else {
            cursor_col + 1 - avail
        };
        let h_end = (h_start + avail).min(virtual_len);

        let before: String = line_chars
            [h_start..cursor_col.min(h_end).min(line_len)]
            .iter()
            .collect();
        spans.push(Span::raw(before));

        let cursor_char = line_chars.get(cursor_col).copied().unwrap_or(' ');
        let blink_on = (tick / 60) % 2 == 0;
        let _ = cursor_glyph; // legacy thin glyph; kept for future fallback
        let cursor_style = if blink_on {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        spans.push(Span::styled(cursor_char.to_string(), cursor_style));

        if cursor_col < line_len {
            let after_start = cursor_col + 1;
            let after: String = line_chars[after_start..h_end.min(line_len)]
                .iter()
                .collect();
            spans.push(Span::raw(after));
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Centred welcome panel rendered in the transcript area when the orc
/// has no real content yet. Below it the persistent compose bar gives
/// the chat affordance; this panel is the orientation copy.
fn render_orc_welcome(frame: &mut Frame, area: Rect, app: &App) {
    use ratatui::widgets::{Block, Borders};
    let block = Block::default().borders(Borders::ALL);
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

    // Vertical centering so the welcome text sits in the middle of the
    // transcript box rather than hugging the top.
    let body_lines: u16 = 5;
    let pad_top = inner.height.saturating_sub(body_lines) / 2;

    let mut lines: Vec<Line> = Vec::new();
    for _ in 0..pad_top {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "◐  orc",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   · {model}"),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        "     parallel claude conversations, orchestrated.",
        Style::default().fg(Color::Gray),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "     describe a task and orc will spawn a worker.",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(vec![
        Span::raw("     "),
        Span::styled("project: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            app.project_dir.display().to_string(),
            Style::default().fg(Color::Gray),
        ),
    ]));
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

    // Insert mode swallows letter hotkeys, so the long action bar would
    // be misleading. Show a single hint pointing to Esc; the full bar
    // reappears the moment we enter nav mode.
    let in_insert = app.focused_compose().map(|c| !c.nav_mode).unwrap_or(false);
    if in_insert {
        let bar = Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Esc",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  for shortcuts", Style::default().fg(Color::DarkGray)),
        ]))
        .style(Style::default().bg(Color::Black));
        frame.render_widget(bar, area);
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
    match app.focused_tab {
        TabId::Orc => {
            // Inline compose owns most keys; surface only what's still
            // relevant. Send/edit affordances are on the compose bar
            // itself; the action bar shows navigation + global escapes.
            parts.push(("⏎", "send"));
            parts.push(("Ctrl+c", "interrupt"));
            let any_review = app.sessions.iter().any(|sv| {
                matches!(sv.session.state, SessionState::AwaitingReview { .. })
            });
            if any_review {
                // `r` is buffered when typing — surface review via the
                // attention cycle instead.
            }
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
                        parts.push(("Ctrl+c", "interrupt"));
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
            parts.push(("!", "next ⚡"));
        }
    }
    parts.push(("n", "claude"));
    parts.push(("?", "scratch"));
    parts.push(("G", "end"));
    parts.push(("h", "help"));
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
