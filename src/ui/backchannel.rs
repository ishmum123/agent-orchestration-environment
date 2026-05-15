// Overlay rendering for the scratch claude backchannel.
//
// Reuses the same `render_event_log` and `render_compose` widgets the
// orc/worker tabs use, so visuals stay unified. The only scratch-specific
// chrome is the centred overlay frame + a one-line hint bar with the
// promotion shortcuts.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::backchannel::Backchannel;
use crate::ui::render_compose;
use crate::ui::worker::{render_event_log, wrapped_line_count};

pub fn render_overlay(frame: &mut Frame, area: Rect, bc: &mut Backchannel, tick: u64) {
    // Generous overlay — leave a small frame so the underlying UI peeks
    // through but the scratch chat gets most of the screen.
    let rect = centered_rect(90, 90, area);
    frame.render_widget(Clear, rect);

    let title = format!(" scratch claude ({}) ", bc.model);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, Style::default().fg(Color::DarkGray)));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    if inner.height < 6 {
        return;
    }

    // Rows: [transcript | compose (5 rows) | hint (1 row)]
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),     // transcript
            Constraint::Length(5),  // compose box
            Constraint::Length(1),  // hint bar
        ])
        .split(inner);

    // Autoscroll: pin to the tail when stick_to_bottom is set.
    let events_area = chunks[0];
    let block_inner_w = events_area.width.saturating_sub(2);
    let inner_h = events_area.height.saturating_sub(2) as usize;
    if bc.stick_to_bottom {
        let total = wrapped_line_count(&bc.history, block_inner_w, &bc.model);
        bc.scroll = total.saturating_sub(inner_h);
    }

    render_event_log(
        frame,
        events_area,
        &bc.history,
        bc.scroll,
        bc.thinking,
        tick,
        &bc.model,
    );

    render_compose(
        frame,
        chunks[1],
        &bc.compose,
        "scratch",
        bc.thinking,
        tick,
    );

    render_hints(frame, chunks[2]);
}

fn render_hints(frame: &mut Frame, area: Rect) {
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
        Span::styled(" attach", dim),
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
