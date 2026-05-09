// Worker tab: structured event-log view with session info and permission log.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{LogEntry, SessionView};
use crate::session::SessionState;

/// Render the worker tab for a single session.
///
/// Header is rendered by `ui::render_header` above this area, so we only
/// need event log + (optional) decisions strip.
pub fn render_worker(
    frame: &mut Frame,
    area: Rect,
    session_view: &SessionView,
    scroll: usize,
    tick: u64,
) {
    let decision_count = session_view.permissions.len().min(8);
    let decisions_height = if decision_count > 0 {
        (decision_count + 2) as u16
    } else {
        0
    };

    let chunks = Layout::vertical([
        Constraint::Min(4),
        Constraint::Length(decisions_height),
    ])
    .split(area);

    render_event_log(
        frame,
        chunks[0],
        &session_view.event_log,
        scroll,
        session_view.is_thinking,
        tick,
    );
    if decisions_height > 0 {
        render_decisions(frame, chunks[1], session_view);
    }
}

#[allow(dead_code)]
fn state_label(state: &SessionState) -> &'static str {
    match state {
        SessionState::Running => "Running",
        SessionState::Blocked { .. } => "Blocked",
        SessionState::AwaitingReview { .. } => "AwaitingReview",
        SessionState::Done { .. } => "Done",
        SessionState::Failed { .. } => "Failed",
    }
}

#[allow(dead_code)]
fn render_session_info(
    frame: &mut Frame,
    area: Rect,
    sv: &SessionView,
    badge: &str,
    badge_color: Color,
    elapsed: &str,
) {
    let s = &sv.session;
    let title = Line::from(vec![
        Span::styled(s.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" \u{b7} "),
        Span::styled(s.model.clone(), Style::default().fg(Color::Cyan)),
    ]);
    let title_right = Line::from(vec![
        Span::styled(
            badge.to_string(),
            Style::default().fg(badge_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(state_label(&s.state), Style::default().fg(badge_color)),
        Span::raw("  "),
        Span::styled(elapsed.to_string(), Style::default().fg(Color::DarkGray)),
    ]);

    let block = Block::default()
        .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
        .title(title)
        .title_alignment(ratatui::layout::Alignment::Left);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let badge_width = title_right.width() as u16;
    if badge_width < inner.width {
        let badge_area = Rect {
            x: inner.x + inner.width - badge_width,
            y: area.y,
            width: badge_width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(title_right), badge_area);
    }

    let lines = vec![
        Line::from(vec![
            Span::styled("  task:      ", Style::default().fg(Color::DarkGray)),
            Span::raw(truncate(&s.task, inner.width.saturating_sub(13) as usize)),
        ]),
        Line::from(vec![
            Span::styled("  worktree:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(truncate(&s.worktree_path, inner.width.saturating_sub(13) as usize)),
        ]),
        Line::from(vec![
            Span::styled("  branch:    ", Style::default().fg(Color::DarkGray)),
            Span::raw(s.branch.clone()),
        ]),
    ];

    let info = Paragraph::new(lines);
    frame.render_widget(info, inner);
}

/// Convert a structured log into renderable lines. Used by both the orc tab
/// and worker tabs — same model for both.
pub fn log_lines(log: &[LogEntry]) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::with_capacity(log.len());
    for entry in log {
        match entry {
            LogEntry::UserText(t) => {
                push_prefixed_lines(
                    &mut out,
                    "you   ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    t,
                    Style::default(),
                );
            }
            LogEntry::OrcInstruction(t) => {
                push_prefixed_lines(
                    &mut out,
                    "orc → ",
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                    t,
                    Style::default(),
                );
            }
            LogEntry::AssistantText(t) => {
                for line in render_markdown(t) {
                    out.push(line);
                }
            }
            LogEntry::Thinking(t) => {
                let trimmed = t.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let lines: Vec<&str> = trimmed.split('\n').collect();
                let cap = 3usize;
                let n = lines.len();
                for (i, ln) in lines.iter().enumerate().take(cap) {
                    let body = if i == 0 {
                        format!("(thinking) {ln}")
                    } else {
                        format!("           {ln}")
                    };
                    out.push(Line::from(Span::styled(
                        body,
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
                if n > cap {
                    out.push(Line::from(Span::styled(
                        format!("           … ({} more lines)", n - cap),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
            }
            LogEntry::ToolUse {
                name,
                input_summary,
            } => out.push(Line::from(vec![
                Span::styled("→ ", Style::default().fg(Color::Yellow)),
                Span::raw(format!("{name}{input_summary}")),
            ])),
            LogEntry::ToolResult { text, is_error } => {
                let color = if *is_error { Color::Red } else { Color::Green };
                let lines: Vec<&str> = text.split('\n').collect();
                let cap = 5usize;
                let body_style = Style::default();
                let arrow_style = Style::default().fg(color);
                let n = lines.len();
                for (i, ln) in lines.iter().enumerate().take(cap) {
                    // Per-line char cap to avoid extreme widths.
                    let trimmed: String = ln.chars().take(200).collect();
                    let trimmed = if ln.chars().count() > 200 {
                        format!("{trimmed}…")
                    } else {
                        trimmed
                    };
                    let prefix = if i == 0 { "← " } else { "  " };
                    out.push(Line::from(vec![
                        Span::styled(prefix.to_string(), arrow_style),
                        Span::styled(trimmed, body_style),
                    ]));
                }
                if n > cap {
                    out.push(Line::from(Span::styled(
                        format!("  … ({} more lines)", n - cap),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            LogEntry::System(s) => {
                for (i, ln) in s.split('\n').enumerate() {
                    let body = if i == 0 {
                        format!("[{ln}")
                    } else {
                        format!(" {ln}")
                    };
                    out.push(Line::from(Span::styled(
                        body,
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                if let Some(last) = out.last_mut() {
                    // Append closing bracket to last line.
                    last.spans.push(Span::styled(
                        "]".to_string(),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
            LogEntry::TurnEnd { cost_usd: _ } => {
                // No textual divider — just a blank line for breathing room.
                out.push(Line::from(""));
            }
            LogEntry::Exploration {
                reads,
                greps,
                lists,
                globs,
            } => {
                let mut parts: Vec<String> = Vec::new();
                let push = |n: u32, label: &str, parts: &mut Vec<String>| {
                    if n > 0 {
                        parts.push(format!(
                            "{} {}",
                            n,
                            if n == 1 { label.trim_end_matches('s') } else { label }
                        ));
                    }
                };
                push(*reads, "reads", &mut parts);
                push(*greps, "greps", &mut parts);
                push(*lists, "lists", &mut parts);
                push(*globs, "globs", &mut parts);
                if parts.is_empty() {
                    continue;
                }
                out.push(Line::from(Span::styled(
                    format!("(explored: {})", parts.join(", ")),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
        }
    }
    out
}

/// Push a multi-line text block, prefixing the first line with `prefix`
/// (in `prefix_style`) and indenting subsequent lines to match.
fn push_prefixed_lines(
    out: &mut Vec<Line<'static>>,
    prefix: &str,
    prefix_style: Style,
    text: &str,
    body_style: Style,
) {
    let indent: String = " ".repeat(prefix.chars().count());
    for (i, ln) in text.split('\n').enumerate() {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(2);
        if i == 0 {
            spans.push(Span::styled(prefix.to_string(), prefix_style));
        } else {
            spans.push(Span::raw(indent.clone()));
        }
        spans.push(Span::styled(ln.to_string(), body_style));
        out.push(Line::from(spans));
    }
}

/// One-line summary of a tool call's input, tool-aware. Falls back to a
/// truncated JSON dump for unknown tools.
pub fn summarize_tool_input(name: &str, input: &serde_json::Value) -> String {
    fn s(v: &serde_json::Value, key: &str) -> Option<String> {
        v.get(key).and_then(|x| x.as_str()).map(String::from)
    }
    fn truncate(s: String, n: usize) -> String {
        if s.chars().count() <= n {
            s
        } else {
            let head: String = s.chars().take(n).collect();
            format!("{head}…")
        }
    }
    let bare = name.strip_prefix("mcp__orc__").unwrap_or(name);
    let summary = match bare {
        "Bash" => s(input, "command").map(|c| truncate(c, 80)),
        "WebFetch" => s(input, "url")
            .or_else(|| s(input, "prompt"))
            .map(|u| truncate(u, 80)),
        "WebSearch" => s(input, "query").map(|q| truncate(q, 80)),
        "Read" | "Edit" | "Write" => s(input, "file_path").map(|p| truncate(p, 80)),
        "Grep" => s(input, "pattern").map(|p| truncate(p, 80)),
        "Glob" => s(input, "pattern").map(|p| truncate(p, 80)),
        "Task" | "Agent" => s(input, "description")
            .or_else(|| s(input, "prompt"))
            .map(|d| truncate(d, 80)),
        _ => None,
    };
    if let Some(s) = summary {
        format!(": {s}")
    } else {
        // Fallback: compact JSON, capped.
        let raw = input.to_string();
        let inner = if raw.chars().count() > 60 {
            format!("{}…", raw.chars().take(57).collect::<String>())
        } else {
            raw
        };
        format!("({inner})")
    }
}

/// Minimal markdown pass for assistant text:
/// - `**x**` → bold styled span
/// - leading `#`/`##`/`###` → bold standalone line, hashes stripped
/// - everything else passes through unstyled
fn render_markdown(text: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for raw in text.split('\n') {
        let trimmed = raw.trim_start();
        if let Some(rest) = strip_heading(trimmed) {
            lines.push(Line::from(Span::styled(
                rest.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        lines.push(Line::from(parse_inline(raw)));
    }
    lines
}

/// Detect `#`–`######` heading prefix; return the body without hashes.
fn strip_heading(s: &str) -> Option<String> {
    let mut hashes = 0;
    for c in s.chars() {
        if c == '#' {
            hashes += 1;
        } else {
            break;
        }
    }
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &s[hashes..];
    if !rest.starts_with(' ') {
        return None;
    }
    Some(rest.trim_start().to_string())
}

/// Inline markdown: `**bold**`, and `[label](url)` rendered as just the
/// label styled cyan + underlined. Unmatched markers pass through.
fn parse_inline(line: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut bold = false;
    let bytes = line.as_bytes();
    let mut i = 0;

    let flush = |buf: &mut String, bold: bool, spans: &mut Vec<Span<'static>>| {
        if !buf.is_empty() {
            let style = if bold {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            spans.push(Span::styled(std::mem::take(buf), style));
        }
    };

    while i < bytes.len() {
        // Try **bold** boundary.
        if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'*' {
            flush(&mut buf, bold, &mut spans);
            bold = !bold;
            i += 2;
            continue;
        }
        // Try [label](url) link.
        if bytes[i] == b'[' {
            if let Some((label, url_end)) = parse_link(&bytes[i..]) {
                let _ = url_end;
                flush(&mut buf, bold, &mut spans);
                let mut style = Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::UNDERLINED);
                if bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                spans.push(Span::styled(label, style));
                i += url_end;
                continue;
            }
        }
        // Plain char (utf-8 safe).
        let ch_start = i;
        let mut end = ch_start + 1;
        while end < bytes.len() && (bytes[end] & 0xC0) == 0x80 {
            end += 1;
        }
        buf.push_str(&line[ch_start..end]);
        i = end;
    }
    flush(&mut buf, bold, &mut spans);
    if spans.is_empty() {
        spans.push(Span::raw(""));
    }
    spans
}

/// Parse a `[label](url)` starting at `bytes[0]`. Returns the label string
/// and the byte length consumed (including closing `)`). Tolerates label
/// containing balanced brackets; URL must not contain `)`.
fn parse_link(bytes: &[u8]) -> Option<(String, usize)> {
    if bytes.first() != Some(&b'[') {
        return None;
    }
    // Find matching `]`.
    let mut depth = 1;
    let mut j = 1;
    while j < bytes.len() {
        match bytes[j] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        j += 1;
    }
    if depth != 0 || j >= bytes.len() {
        return None;
    }
    // Expect `(` immediately after `]`.
    if bytes.get(j + 1) != Some(&b'(') {
        return None;
    }
    // Find matching `)`.
    let mut k = j + 2;
    while k < bytes.len() && bytes[k] != b')' {
        k += 1;
    }
    if k >= bytes.len() {
        return None;
    }
    let label = std::str::from_utf8(&bytes[1..j]).ok()?.to_string();
    // Replace newlines in label (markdown links can wrap source).
    let label = label.replace('\n', " ");
    Some((label, k + 1))
}

pub fn render_event_log(
    frame: &mut Frame,
    area: Rect,
    log: &[LogEntry],
    scroll: usize,
    thinking: bool,
    tick: u64,
) {
    // Compute first to use its line_count for the scroll indicator.
    let mut lines = log_lines(log);
    if thinking {
        lines.push(thinking_line(tick));
    }
    let total_lines = lines.len();

    let block_inner_w = area.width.saturating_sub(2);
    let para_for_count = Paragraph::new(lines.clone())
        .wrap(Wrap { trim: false });
    let wrapped = para_for_count.line_count(block_inner_w) as usize;

    let inner_h = area.height.saturating_sub(2) as usize;
    let max_scroll = wrapped.saturating_sub(inner_h);
    let scroll_clamped = scroll.min(max_scroll);

    let title = format_events_title(scroll_clamped, inner_h, wrapped, total_lines);

    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        title,
        Style::default().fg(Color::DarkGray),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Force-clear inner cells first — Paragraph with Wrap+scroll can leave
    // stray cells from prior frames (especially after tab switches).
    frame.render_widget(Clear, inner);

    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll_clamped as u16, 0));
    frame.render_widget(para, inner);
}

/// Build " events  [N–M / TOTAL] " title.
fn format_events_title(
    scroll: usize,
    visible: usize,
    wrapped_total: usize,
    entries_total: usize,
) -> String {
    if wrapped_total == 0 {
        return " events ".to_string();
    }
    let start = scroll + 1;
    let end = (scroll + visible.max(1)).min(wrapped_total);
    format!(
        " events  [{start}–{end} / {wrapped_total} lines · {entries_total} entries] "
    )
}

/// Build the spinner line shown at the tail while an agent is in a
/// thinking phase. Animated via `tick` so each frame shows a new glyph.
fn thinking_line(tick: u64) -> Line<'static> {
    const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let g = FRAMES[(tick / 4) as usize % FRAMES.len()];
    Line::from(vec![
        Span::styled(
            format!("{g} "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "thinking",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
    ])
}

/// Compute how many wrapped lines `log` produces at the given inner width.
/// Used by the autoscroll path to pin the view to the bottom.
pub fn wrapped_line_count(log: &[LogEntry], inner_width: u16) -> usize {
    let lines = log_lines(log);
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .line_count(inner_width) as usize
}

fn render_decisions(frame: &mut Frame, area: Rect, sv: &SessionView) {
    if area.height < 2 {
        return;
    }

    let max_entries = (area.height as usize).saturating_sub(1);
    let start = sv.permissions.len().saturating_sub(max_entries);
    let entries = &sv.permissions[start..];

    let mut lines: Vec<Line> = Vec::with_capacity(entries.len() + 1);
    lines.push(Line::from(Span::styled(
        "recent decisions:",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::UNDERLINED),
    )));

    for entry in entries {
        let elapsed_secs = entry.timestamp.elapsed().as_secs();
        let time_str = if elapsed_secs < 60 {
            format!("{elapsed_secs}s ago")
        } else {
            format!("{}m ago", elapsed_secs / 60)
        };

        let decision_style = if entry.decision.starts_with('\u{2713}')
            || entry.decision.contains("allow")
            || entry.decision.contains("approved")
        {
            Style::default().fg(Color::Green)
        } else if entry.decision.contains("deny") || entry.decision.contains("denied") {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Yellow)
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<8}", time_str),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(truncate(&entry.request, 28)),
            Span::raw("  "),
            Span::styled(entry.decision.clone(), decision_style),
            Span::raw(" "),
            Span::styled(
                format!("({})", entry.decided_by),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len > 3 {
        format!("{}...", &s[..max_len - 3])
    } else {
        s[..max_len].to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::PermissionEntry;
    use crate::session::{Session, SessionMode, SessionState};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::Instant;

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf.cell((x, y)).unwrap().symbol());
            }
        }
        out
    }

    fn test_session_view() -> SessionView {
        let session = Session {
            id: "sess-001".to_string(),
            name: "explore-agents".to_string(),
            task: "investigate auth-service oauth flow".to_string(),
            worktree_path: "~/.config/orc/wt/explore-agents-a1b2/".to_string(),
            branch: "orc/explore-agents-a1b2".to_string(),
            base_commit: "abc1234".to_string(),
            tmux_session: "orc-explore-agents".to_string(),
            state: SessionState::Running,
            mode: SessionMode::Watch,
            model: "sonnet".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ended_at: None,
            claude_session_id: None,
        };

        let event_log = vec![
            LogEntry::AssistantText("looking at auth-service/src/oauth/".into()),
            LogEntry::ToolUse {
                name: "Read".into(),
                input_summary: "callback.rs".into(),
            },
            LogEntry::ToolResult {
                text: "ok".into(),
                is_error: false,
            },
        ];

        let permissions = vec![
            PermissionEntry {
                timestamp: Instant::now(),
                request: "read in repo".to_string(),
                decision: "allow".to_string(),
                decided_by: "policy: allow.read".to_string(),
            },
            PermissionEntry {
                timestamp: Instant::now(),
                request: "bash: cargo test".to_string(),
                decision: "allow".to_string(),
                decided_by: "haiku, conf 0.92".to_string(),
            },
        ];

        SessionView {
            session,
            event_log,
            permissions,
            tab_index: 0,
            claude_session_id: None,
            skip_next_tool_result: false,
            stick_to_bottom: true,
            is_thinking: false,
        }
    }

    #[test]
    fn render_worker_does_not_panic() {
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let sv = test_session_view();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv, 0, 0);
            })
            .unwrap();
    }

    #[test]
    fn render_worker_tiny_area_no_panic() {
        let backend = TestBackend::new(20, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let sv = test_session_view();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv, 0, 0);
            })
            .unwrap();
    }

    #[test]
    fn render_worker_no_permissions() {
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut sv = test_session_view();
        sv.permissions.clear();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv, 0, 0);
            })
            .unwrap();
    }

    #[test]
    fn render_worker_empty_event_log() {
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut sv = test_session_view();
        sv.event_log.clear();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv, 0, 0);
            })
            .unwrap();
    }

    #[test]
    fn render_worker_contains_session_name() {
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let sv = test_session_view();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv, 0, 0);
            })
            .unwrap();
        let content = buffer_text(&terminal);
        // Worker tab no longer renders its own session-info header — the
        // header is now rendered by ui::render_header above this area, and
        // session name appears in the agents panel. Keep the smoke test
        // by checking event log content.
        assert!(content.contains("looking at auth-service"), "{}", content);
    }

    #[test]
    fn render_worker_contains_decisions() {
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let sv = test_session_view();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_worker(frame, area, &sv, 0, 0);
            })
            .unwrap();
        let content = buffer_text(&terminal);
        assert!(content.contains("recent decisions"));
    }

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string() {
        assert_eq!(truncate("hello world", 8), "hello...");
    }

    #[test]
    fn state_label_variants() {
        assert_eq!(state_label(&SessionState::Running), "Running");
    }
}
