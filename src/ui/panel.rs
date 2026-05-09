// Persistent right-side agents panel.
//
// Always visible on every tab. Renders one card per agent (orc + workers),
// 3 lines + blank between. Highlights focused tab's card.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{App, TabId};

/// Fixed panel width in columns.
pub const PANEL_WIDTH: u16 = 32;

pub fn render_panel(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let inner_w = inner.width as usize;

    // Orc card (always first).
    let focused = matches!(app.focused_tab, TabId::Orc);
    let orc_badge = if app.orc_view.alive { "◐" } else { "✗" };
    let orc_summary = app
        .session_summary("orc")
        .unwrap_or_else(|| "orchestrator".to_string());
    push_card(
        &mut lines,
        focused,
        orc_badge,
        Color::Cyan,
        "orc",
        &orc_summary,
        "alive",
        &elapsed_str_secs(app.started_at.elapsed().as_secs()),
        inner_w,
    );

    // Worker cards.
    for (idx, sv) in app.sessions.iter().enumerate() {
        let is_focused = matches!(app.focused_tab, TabId::Worker(i) if i == idx);
        let (badge, color) = App::state_badge(&sv.session.state);
        let elapsed = App::elapsed_str(&sv.session);
        let state_label = match &sv.session.state {
            crate::session::SessionState::Running => "running",
            crate::session::SessionState::Blocked { .. } => "blocked",
            crate::session::SessionState::AwaitingReview { .. } => "review",
            crate::session::SessionState::Done { .. } => "done",
            crate::session::SessionState::Failed { .. } => "failed",
        };
        let summary = app
            .session_summary(&sv.session.id)
            .unwrap_or_else(|| sv.session.task.clone());
        push_card(
            &mut lines,
            is_focused,
            badge,
            color,
            &sv.session.name,
            &summary,
            state_label,
            &elapsed,
            inner_w,
        );
    }

    // No wrap: each card line is pre-truncated to fit. Letting Paragraph
    // wrap would re-flow long names/summaries and misalign cards.
    let para = Paragraph::new(lines);
    frame.render_widget(para, inner);
}

#[allow(clippy::too_many_arguments)]
fn push_card(
    lines: &mut Vec<Line<'static>>,
    focused: bool,
    badge: &str,
    badge_color: Color,
    name: &str,
    summary: &str,
    state_label: &str,
    elapsed: &str,
    width: usize,
) {
    let bar = if focused { "▌" } else { " " };
    let bar_style = if focused {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let name_style = if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    // Line 1: bar + space + badge + space + name. bar(1)+space(1)+badge(1)+space(1) = 4 cols.
    let name_avail = width.saturating_sub(4);
    let name_trunc = truncate(name, name_avail);
    lines.push(Line::from(vec![
        Span::styled(bar.to_string(), bar_style),
        Span::raw(" "),
        Span::styled(
            badge.to_string(),
            Style::default()
                .fg(badge_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(name_trunc, name_style),
    ]));

    // Lines 2-3: summary truncated to two lines.
    let avail = width.saturating_sub(4); // 2-char indent + bar + space
    let (s1, s2) = wrap_two(summary, avail);
    lines.push(Line::from(vec![
        Span::styled(bar.to_string(), bar_style),
        Span::raw("   "),
        Span::styled(s1, Style::default().fg(Color::Gray)),
    ]));
    if !s2.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(bar.to_string(), bar_style),
            Span::raw("   "),
            Span::styled(s2, Style::default().fg(Color::Gray)),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled(bar.to_string(), bar_style),
            Span::raw("   "),
        ]));
    }

    // Line 4: state · elapsed (state colored by badge; · and elapsed dim).
    // Truncate state_label first if it would overflow on its own; assume
    // typical state labels are short enough.
    let state_truncated = truncate(state_label, avail.saturating_sub(2));
    let elapsed_room = avail.saturating_sub(state_truncated.chars().count() + 3);
    let elapsed_truncated = truncate(elapsed, elapsed_room);
    lines.push(Line::from(vec![
        Span::styled(bar.to_string(), bar_style),
        Span::raw("   "),
        Span::styled(state_truncated, Style::default().fg(badge_color)),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled(elapsed_truncated, Style::default().fg(Color::DarkGray)),
    ]));

    // Blank separator
    lines.push(Line::from(""));
}

/// Truncate a string to at most `width` *characters*, appending `…` if cut.
fn truncate(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= width {
        return s.to_string();
    }
    let take = width.saturating_sub(1);
    let mut out: String = s.chars().take(take).collect();
    out.push('…');
    out
}

fn wrap_two(s: &str, width: usize) -> (String, String) {
    if width == 0 {
        return (String::new(), String::new());
    }
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= width {
        return (collapsed, String::new());
    }
    // Word-aware split: break line 1 at the last whitespace ≤ width.
    let line1 = take_first_line(&collapsed, width);
    let consumed = line1.chars().count();
    // Skip the breaking space if there was one.
    let rest_start = if collapsed.chars().nth(consumed) == Some(' ') {
        consumed + 1
    } else {
        consumed
    };
    let rest: String = collapsed.chars().skip(rest_start).collect();
    if rest.chars().count() <= width {
        (line1, rest)
    } else {
        let line2: String = rest
            .chars()
            .take(width.saturating_sub(1))
            .collect::<String>()
            + "…";
        (line1, line2)
    }
}

/// Take the longest prefix of `s` that fits in `width` chars, breaking at
/// whitespace if possible. If no whitespace before `width` (i.e. the first
/// word is itself longer than width), hard-cut at the width boundary.
fn take_first_line(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= width {
        return s.to_string();
    }
    // Find last space strictly before width (so we don't include the space
    // itself in the line). Skip leading spaces — if the first non-space
    // word is longer than width, fall through to hard-cut.
    let mut break_at: Option<usize> = None;
    for (i, c) in chars.iter().enumerate().take(width) {
        if *c == ' ' && i > 0 {
            break_at = Some(i);
        }
    }
    let cut = break_at.unwrap_or(width);
    chars[..cut].iter().collect()
}

fn elapsed_str_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m", secs / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Session, SessionMode, SessionState};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn buf_text(t: &Terminal<TestBackend>) -> String {
        let buf = t.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf.cell((x, y)).unwrap().symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn panel_renders_orc_card() {
        let backend = TestBackend::new(40, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::new(".");
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, PANEL_WIDTH, 20);
                render_panel(f, area, &app);
            })
            .unwrap();
        let s = buf_text(&terminal);
        assert!(s.contains("orc"), "{}", s);
        assert!(s.contains("orchestrator"), "{}", s);
    }

    #[test]
    fn panel_renders_on_worker_focused_tab() {
        // Panel content is identical regardless of which tab is focused —
        // it renders every agent (orc + workers) every time.
        let backend = TestBackend::new(40, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(".");
        app.add_session(Session {
            id: "s1".into(),
            name: "alpha".into(),
            task: "t".into(),
            worktree_path: "/tmp".into(),
            branch: "br".into(),
            base_commit: "abc".into(),
            tmux_session: "orc-x".into(),
            state: SessionState::Running,
            mode: SessionMode::Watch,
            model: "sonnet".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ended_at: None,
            claude_session_id: None,
        });
        app.focus_tab(crate::app::TabId::Worker(0));
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, PANEL_WIDTH, 30);
                render_panel(f, area, &app);
            })
            .unwrap();
        let s = buf_text(&terminal);
        assert!(s.contains("orc"), "{}", s);
        assert!(s.contains("alpha"), "{}", s);
    }

    #[test]
    fn panel_includes_workers() {
        let backend = TestBackend::new(40, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(".");
        app.add_session(Session {
            id: "s1".into(),
            name: "explorer".into(),
            task: "look around".into(),
            worktree_path: "/tmp".into(),
            branch: "br".into(),
            base_commit: "abc".into(),
            tmux_session: "orc-x".into(),
            state: SessionState::Running,
            mode: SessionMode::Watch,
            model: "sonnet".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ended_at: None,
            claude_session_id: None,
        });
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, PANEL_WIDTH, 30);
                render_panel(f, area, &app);
            })
            .unwrap();
        let s = buf_text(&terminal);
        assert!(s.contains("explorer"), "{}", s);
        assert!(s.contains("running"), "{}", s);
    }

    #[test]
    fn wrap_two_short() {
        assert_eq!(wrap_two("hi", 10), ("hi".to_string(), "".to_string()));
    }

    #[test]
    fn wrap_two_long_truncates() {
        let (a, b) = wrap_two(&"x".repeat(100), 10);
        assert_eq!(a.chars().count(), 10);
        assert!(b.ends_with('…'));
    }
}
