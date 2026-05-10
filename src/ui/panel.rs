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
    let orc_ctx = context_pct(app.orc_view.last_context_tokens, &app.orc_view.model);
    push_card(
        &mut lines,
        focused,
        orc_badge,
        Color::Cyan,
        &name_with_model("orc", &app.orc_view.model),
        &orc_summary,
        "alive",
        &elapsed_str_secs(app.started_at.elapsed().as_secs()),
        orc_ctx,
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
        let ctx = context_pct(sv.last_context_tokens, &sv.session.model);
        push_card(
            &mut lines,
            is_focused,
            badge,
            color,
            &name_with_model(&sv.session.name, &sv.session.model),
            &summary,
            state_label,
            &elapsed,
            ctx,
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
    ctx: Option<ContextDisplay>,
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

    // Line 4: state · elapsed [· Nk · pct% ⚠]. Context segment only shown
    // when occupancy crosses the warn threshold; ⚠ color escalates at the
    // alarm threshold. claude auto-compacts at 50%, so the indicator
    // climbs and then drops on its own.
    let state_truncated = truncate(state_label, avail.saturating_sub(2));
    let mut spans = vec![
        Span::styled(bar.to_string(), bar_style),
        Span::raw("   "),
        Span::styled(state_truncated.clone(), Style::default().fg(badge_color)),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
    ];
    let used_so_far = state_truncated.chars().count() + 3;
    if let Some(ctx) = ctx {
        let ctx_str = format!(" · {} · {}%", ctx.tokens_label, ctx.pct);
        let glyph = " ⚠";
        let ctx_room = avail.saturating_sub(used_so_far + ctx_str.chars().count() + glyph.chars().count());
        let elapsed_truncated = truncate(elapsed, ctx_room);
        spans.push(Span::styled(elapsed_truncated, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(ctx_str, Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(glyph.to_string(), Style::default().fg(ctx.glyph_color).add_modifier(Modifier::BOLD)));
    } else {
        let elapsed_room = avail.saturating_sub(used_so_far);
        let elapsed_truncated = truncate(elapsed, elapsed_room);
        spans.push(Span::styled(elapsed_truncated, Style::default().fg(Color::DarkGray)));
    }
    lines.push(Line::from(spans));

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

/// Display tier for the context-window indicator. Yellow at the warn
/// threshold, red at the alarm threshold (one notch before claude's
/// 50% auto-compact).
const CTX_WARN_PCT: u32 = 30;
const CTX_ALARM_PCT: u32 = 45;

#[derive(Debug, Clone, Copy)]
struct ContextDisplay {
    tokens_label: TokensLabel,
    pct: u32,
    glyph_color: Color,
}

#[derive(Debug, Clone, Copy)]
struct TokensLabel(u64);

impl std::fmt::Display for TokensLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Round to nearest 1k: e.g. 350432 → "350k".
        let k = (self.0 + 500) / 1000;
        write!(f, "{k}k")
    }
}

fn context_pct(tokens: Option<u64>, model: &str) -> Option<ContextDisplay> {
    let t = tokens?;
    let cap = crate::worker::context_cap_for(model);
    if cap == 0 {
        return None;
    }
    let pct = ((t as u128 * 100) / cap as u128) as u32;
    if pct < CTX_WARN_PCT {
        return None;
    }
    let glyph_color = if pct >= CTX_ALARM_PCT {
        Color::Red
    } else {
        Color::Yellow
    };
    Some(ContextDisplay {
        tokens_label: TokensLabel(t),
        pct,
        glyph_color,
    })
}

fn name_with_model(name: &str, model: &str) -> String {
    if model.is_empty() {
        name.to_string()
    } else {
        format!("{name} ({model})")
    }
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
    fn context_pct_hidden_below_warn() {
        // 200k of 1M opus = 20%, below 30% warn threshold.
        assert!(context_pct(Some(200_000), "opus").is_none());
    }

    #[test]
    fn context_pct_yellow_at_warn() {
        // 350k of 1M = 35% → shown, yellow.
        let d = context_pct(Some(350_000), "opus").expect("shown");
        assert_eq!(d.pct, 35);
        assert_eq!(d.glyph_color, Color::Yellow);
        assert_eq!(format!("{}", d.tokens_label), "350k");
    }

    #[test]
    fn context_pct_red_at_alarm() {
        // 470k of 1M = 47% → shown, red.
        let d = context_pct(Some(470_000), "opus").expect("shown");
        assert_eq!(d.pct, 47);
        assert_eq!(d.glyph_color, Color::Red);
    }

    #[test]
    fn context_pct_uses_haiku_cap() {
        // 100k of 200k haiku = 50% → shown, red.
        let d = context_pct(Some(100_000), "haiku").expect("shown");
        assert_eq!(d.pct, 50);
        assert_eq!(d.glyph_color, Color::Red);
    }

    #[test]
    fn context_pct_none_when_no_tokens() {
        assert!(context_pct(None, "opus").is_none());
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
