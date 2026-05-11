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
    let orc_color = if app.orc_view.alive { Color::Cyan } else { Color::Red };
    let orc_summary = app
        .session_summary("orc")
        .unwrap_or_else(|| "orchestrator".to_string());
    let orc_ctx = context_pct(app.orc_view.last_context_tokens, &app.orc_view.model);
    push_card(
        &mut lines,
        focused,
        orc_badge,
        orc_color,
        "orc",
        &app.orc_view.model,
        &orc_summary,
        if app.orc_view.alive { "" } else { "exited" },
        &elapsed_str_secs(app.started_at.elapsed().as_secs()),
        orc_ctx,
        inner_w,
    );

    // Worker cards.
    for (idx, sv) in app.sessions.iter().enumerate() {
        let is_focused = matches!(app.focused_tab, TabId::Worker(i) if i == idx);
        let (badge, color) = App::state_badge(&sv.session.state);
        let elapsed = App::elapsed_str(&sv.session);
        // Only show a state tag when the state isn't "running" — running
        // is already encoded in the badge.
        let state_label = match &sv.session.state {
            crate::session::SessionState::Running => "",
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
            &sv.session.name,
            &sv.session.model,
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

/// Render a 4-5 line agent card. Layout:
///
///   ▌ ◐ {name}                              [{state-tag-if-not-running}]
///   ▌   {model} · {elapsed} [· {tokens} · {pct}%]
///   ▌   {summary l1}
///   ▌   {summary l2}     (only when summary wraps)
///   [blank]
///
/// Name always lives on line 1 by itself (truncated with `…` only if it
/// itself overflows the panel width). Model/elapsed/context always live
/// on line 2 so they're never elided by a long name.
#[allow(clippy::too_many_arguments)]
fn push_card(
    lines: &mut Vec<Line<'static>>,
    focused: bool,
    badge: &str,
    badge_color: Color,
    name: &str,
    model: &str,
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
    let name_style = Style::default().add_modifier(Modifier::BOLD);

    let show_state = !state_label.is_empty();
    let state_w = if show_state { state_label.chars().count() + 1 } else { 0 };
    let prefix_w = 4; // bar(1) sp(1) badge(1) sp(1)

    // ---- Line 1: ▌ badge name [pad] state-tag ----
    let name_avail = width.saturating_sub(prefix_w).saturating_sub(state_w);
    let name_trunc = truncate(name, name_avail.max(1));
    let used = prefix_w + name_trunc.chars().count();
    let pad = width.saturating_sub(used + state_w);
    let mut row1: Vec<Span<'static>> = vec![
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
    ];
    if show_state {
        row1.push(Span::raw(" ".repeat(pad)));
        row1.push(Span::styled(
            state_label.to_string(),
            Style::default().fg(badge_color).add_modifier(Modifier::BOLD),
        ));
    }
    lines.push(Line::from(row1));

    // ---- Line 2: ▌   model · elapsed [· tokens · pct%] ----
    let mut row2: Vec<Span<'static>> = vec![
        Span::styled(bar.to_string(), bar_style),
        Span::raw("   "),
    ];
    if !model.is_empty() {
        row2.push(Span::styled(
            model.to_string(),
            Style::default().fg(Color::Cyan),
        ));
        row2.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
    }
    row2.push(Span::styled(
        elapsed.to_string(),
        Style::default().fg(Color::DarkGray),
    ));
    if let Some(ctx) = ctx {
        row2.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        row2.push(Span::styled(
            format!("{}", ctx.tokens_label),
            Style::default().fg(Color::DarkGray),
        ));
        row2.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        row2.push(Span::styled(
            format!("{}%", ctx.pct),
            Style::default().fg(ctx.glyph_color).add_modifier(Modifier::BOLD),
        ));
    }
    lines.push(Line::from(row2));

    // -------------------- Summary (up to 2 wrapped lines) --------------------
    let summary_avail = width.saturating_sub(4);
    let collapsed: String = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    if !collapsed.is_empty() && summary_avail > 0 {
        let (s1, s2) = wrap_two(&collapsed, summary_avail);
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
        }
    }

    // Blank separator between cards.
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

/// Context-occupancy display threshold. Below 20% we hide the indicator
/// entirely — the agent has plenty of room and the column is just noise.
/// At 20%+ we surface it (DarkGray), escalating to Yellow at the warn
/// threshold and Red near auto-compaction.
const CTX_SHOW_PCT: u32 = 20;

fn context_pct(tokens: Option<u64>, model: &str) -> Option<ContextDisplay> {
    let t = tokens?;
    if t == 0 {
        return None;
    }
    let cap = crate::worker::context_cap_for(model);
    if cap == 0 {
        return None;
    }
    let pct = ((t as u128 * 100) / cap as u128) as u32;
    if pct < CTX_SHOW_PCT {
        return None;
    }
    let glyph_color = if pct >= CTX_ALARM_PCT {
        Color::Red
    } else if pct >= CTX_WARN_PCT {
        Color::Yellow
    } else {
        Color::DarkGray
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
    crate::app::format_elapsed(secs)
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
        // Running sessions encode state in the ◐ badge; the word "running"
        // is redundant and intentionally not rendered.
        assert!(s.contains("◐"), "{}", s);
    }

    #[test]
    fn context_pct_dim_at_show_threshold() {
        // 20% is the show threshold — surface, DarkGray.
        let d = context_pct(Some(200_000), "opus").expect("shown");
        assert_eq!(d.pct, 20);
        assert_eq!(d.glyph_color, Color::DarkGray);
    }

    #[test]
    fn context_pct_hidden_below_show_threshold() {
        // 19% (190k of 1M opus) — hidden.
        assert!(context_pct(Some(190_000), "opus").is_none());
        // And of course tiny values.
        assert!(context_pct(Some(500), "opus").is_none());
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
