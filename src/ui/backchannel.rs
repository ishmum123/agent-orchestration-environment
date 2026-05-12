// Overlay rendering for the scratch claude backchannel. ChatGPT-style:
// scrollable transcript on top, multi-line input box at the bottom.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::backchannel::{Backchannel, BackchannelEntry};

const BORDER: Color = Color::Cyan;

pub fn render_overlay(frame: &mut Frame, area: Rect, bc: &Backchannel, tick: u64) {
    // 75% of screen, centred.
    let rect = centered_rect(75, 80, area);
    frame.render_widget(Clear, rect);

    let title = format!(" scratch claude ({}) ", bc.model);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER))
        .title(Span::styled(
            title,
            Style::default()
                .fg(BORDER)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    if inner.height < 5 {
        return;
    }

    // Reserve 5 rows for the input box (border + 3 content rows + hint
    // line), 1 for the hint above it. Transcript fills the rest. When
    // attachments are staged, a 1-row chip strip is inserted above the
    // input box.
    let chip_rows: u16 = if bc.attachments.is_empty() { 0 } else { 1 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),               // transcript
            Constraint::Length(chip_rows),    // chip strip (0 or 1)
            Constraint::Length(5),            // input box
            Constraint::Length(1),            // hint bar
        ])
        .split(inner);

    render_transcript(frame, chunks[0], bc, tick);
    if chip_rows > 0 {
        render_chips(frame, chunks[1], bc);
    }
    render_input(frame, chunks[2], &bc.input);
    render_hints(frame, chunks[3], bc.thinking);
}

fn render_chips(frame: &mut Frame, area: Rect, bc: &Backchannel) {
    const MAX: usize = 6;
    let total = bc.attachments.len();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut shown = 0usize;
    for a in bc.attachments.iter().take(MAX) {
        let name = truncate(&a.display_name, 24);
        spans.push(Span::styled(
            format!(" [{name} ✕] "),
            Style::default().fg(Color::Cyan),
        ));
        shown += 1;
    }
    if total > shown {
        spans.push(Span::styled(
            format!(" +{} more ", total - shown),
            Style::default().fg(Color::DarkGray),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

fn render_transcript(frame: &mut Frame, area: Rect, bc: &Backchannel, tick: u64) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if bc.history.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  ask sonnet anything. orc never sees this conversation.",
            Style::default().fg(Color::DarkGray),
        )));
    }
    for entry in &bc.history {
        match entry {
            BackchannelEntry::User(t) => {
                lines.push(Line::from(Span::styled(
                    "you",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )));
                for l in t.lines() {
                    lines.push(Line::from(Span::raw(l.to_string())));
                }
                lines.push(Line::from(""));
            }
            BackchannelEntry::Assistant(t) => {
                lines.push(Line::from(Span::styled(
                    "claude",
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                )));
                for l in t.lines() {
                    lines.push(Line::from(Span::raw(l.to_string())));
                }
                lines.push(Line::from(""));
            }
            BackchannelEntry::Tool(t) => {
                lines.push(Line::from(Span::styled(
                    t.clone(),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            BackchannelEntry::System(t) => {
                lines.push(Line::from(Span::styled(
                    t.clone(),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    }
    if bc.thinking {
        let spinner = ["◐", "◓", "◑", "◒"];
        let idx = ((tick / 5) as usize) % spinner.len();
        lines.push(Line::from(Span::styled(
            format!("{} thinking", spinner[idx]),
            Style::default().fg(Color::Yellow),
        )));
    }

    // Wrapping is handled by Paragraph::wrap. Scroll is line offset.
    let total = lines.len() as u16;
    let h = area.height;
    let max_scroll = total.saturating_sub(h);
    let scroll: u16 = if bc.stick_to_bottom {
        max_scroll
    } else {
        (bc.scroll as u16).min(max_scroll)
    };

    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(para, area);
}

fn render_input(frame: &mut Frame, area: Rect, buffer: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if buffer.is_empty() {
        let line = Line::from(vec![
            Span::styled("_", Style::default().fg(Color::White)),
            Span::styled(
                " ask anything — shift+enter newline, enter to send",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ),
        ]);
        frame.render_widget(Paragraph::new(line).wrap(Wrap { trim: false }), inner);
        return;
    }
    let text = format!("{buffer}_");
    frame.render_widget(
        Paragraph::new(text)
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn render_hints(frame: &mut Frame, area: Rect, _thinking: bool) {
    let dim = Style::default().fg(Color::DarkGray);
    let key = Style::default().fg(Color::Yellow);
    let hints = Line::from(vec![
        Span::styled("enter", key),
        Span::styled(" send  ", dim),
        Span::styled("shift+enter", key),
        Span::styled(" newline  ", dim),
        Span::styled("esc", key),
        Span::styled(" close  ", dim),
        Span::styled("Ctrl+x", key),
        Span::styled(" kill  ", dim),
        Span::styled("Ctrl+b", key),
        Span::styled(" → opus  ", dim),
        Span::styled("Ctrl+n", key),
        Span::styled(" → orc", dim),
    ]);
    frame.render_widget(Paragraph::new(hints), area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}
