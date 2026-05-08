// Modal overlays: confirm dialogs, help screens, task input, permission prompts.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::app::{Modal, TabId};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn render_modal(frame: &mut Frame, area: Rect, modal: &Modal) {
    match modal {
        Modal::NewTask { target, buffer } => render_new_task(frame, area, *target, buffer),
        Modal::AskUser {
            session_id,
            question_id: _,
            question,
            context,
            buffer,
        } => render_ask_user(frame, area, session_id, question, context.as_deref(), buffer),
        Modal::Comment { file, line, buffer, .. } => {
            render_comment(frame, area, file, *line, buffer)
        }
        Modal::ConfirmKill { session_id: _, name } => render_confirm_kill(frame, area, name),
        Modal::ConfirmQuit => render_confirm_quit(frame, area),
        Modal::Help => render_help(frame, area),
    }
}

// ---------------------------------------------------------------------------
// Geometry helper
// ---------------------------------------------------------------------------

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

/// Fixed-size centered rect (clamped to area).
fn centered_fixed(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

// ---------------------------------------------------------------------------
// Shared styles
// ---------------------------------------------------------------------------

const BORDER_COLOR: Color = Color::Cyan;
const DIM: Style = Style::new().fg(Color::DarkGray);

fn modal_block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER_COLOR))
        .title(format!(" {} ", title))
        .title_style(Style::default().fg(BORDER_COLOR).add_modifier(Modifier::BOLD))
}

fn hint_line<'a>(pairs: &[(&'a str, &'a str)]) -> Line<'a> {
    let mut spans: Vec<Span<'a>> = Vec::new();
    for (i, (key, action)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("       ", DIM));
        }
        spans.push(Span::styled(*key, Style::default().fg(Color::Yellow)));
        spans.push(Span::styled(format!(" {action}"), DIM));
    }
    Line::from(spans)
}

/// Split inner area into body + hint bar (last line).
fn split_body_hint(inner: Rect) -> (Rect, Rect) {
    if inner.height < 2 {
        return (inner, Rect::default());
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    (chunks[0], chunks[1])
}

// ---------------------------------------------------------------------------
// Input box helper — renders a bordered text area with cursor
// ---------------------------------------------------------------------------

fn render_input_box(frame: &mut Frame, area: Rect, buffer: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let text = format!("{buffer}_");
    let paragraph = Paragraph::new(text)
        .style(Style::default().fg(Color::White))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

// ---------------------------------------------------------------------------
// New task modal
// ---------------------------------------------------------------------------

fn render_new_task(frame: &mut Frame, area: Rect, target: TabId, buffer: &str) {
    let rect = centered_rect(60, 40, area);
    frame.render_widget(Clear, rect);

    let show_welcome = matches!(target, TabId::Orc) && buffer.is_empty();

    let title = match target {
        TabId::Orc => "speak to orc".to_string(),
        TabId::Worker(idx) => format!("speak to worker #{}", idx + 1),
    };
    let block = modal_block(&title);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let (body, hint_area) = split_body_hint(inner);

    // Welcome header takes 2 lines (wrapped blurb) when present.
    let welcome_height: u16 = if show_welcome { 3 } else { 0 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(welcome_height),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(3),
        ])
        .split(body);

    if show_welcome {
        let welcome = Paragraph::new(
            "hey I am orc — give me a task or a list of tasks and I'll help you complete them",
        )
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .wrap(Wrap { trim: false });
        frame.render_widget(welcome, chunks[0]);
    }

    let label_text = match target {
        TabId::Orc => "describe the task for orc to plan:",
        TabId::Worker(_) => "send a message to this worker:",
    };
    let label = Paragraph::new(label_text).style(Style::default().fg(Color::White));
    frame.render_widget(label, chunks[1]);

    render_input_box(frame, chunks[3], buffer);

    let hints = hint_line(&[
        ("enter", "send"),
        ("shift+enter", "newline"),
        ("esc", "cancel"),
    ]);
    frame.render_widget(Paragraph::new(hints), hint_area);
}

fn render_comment(frame: &mut Frame, area: Rect, file: &str, line: usize, buffer: &str) {
    let rect = centered_rect(60, 30, area);
    frame.render_widget(Clear, rect);

    let title = format!("comment on {file}:{line}");
    let block = modal_block(&title);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let (body, hint_area) = split_body_hint(inner);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3)])
        .split(body);

    let label = Paragraph::new("review note (sent to worker on submit):")
        .style(Style::default().fg(Color::White));
    frame.render_widget(label, chunks[0]);

    render_input_box(frame, chunks[1], buffer);

    let hints = hint_line(&[("enter", "save"), ("esc", "cancel")]);
    frame.render_widget(Paragraph::new(hints), hint_area);
}

// ---------------------------------------------------------------------------
// Ask user modal
// ---------------------------------------------------------------------------

fn render_ask_user(
    frame: &mut Frame,
    area: Rect,
    session_id: &str,
    question: &str,
    context: Option<&str>,
    buffer: &str,
) {
    let rect = centered_rect(70, 40, area);
    frame.render_widget(Clear, rect);

    let block = modal_block("orc needs your input");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let (body, hint_area) = split_body_hint(inner);

    // Count content lines: session + optional context + question + spacer + input
    let context_lines: u16 = if context.is_some() { 2 } else { 0 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),              // session id
            Constraint::Length(context_lines),   // context (0 or 2)
            Constraint::Length(1),              // question
            Constraint::Length(1),              // spacer
            Constraint::Min(3),                // input box
        ])
        .split(body);

    let session_line = Paragraph::new(Line::from(vec![
        Span::styled("session: ", DIM),
        Span::styled(session_id, Style::default().fg(Color::Cyan)),
    ]));
    frame.render_widget(session_line, chunks[0]);

    if let Some(ctx) = context {
        let ctx_para = Paragraph::new(ctx)
            .style(DIM)
            .wrap(Wrap { trim: false });
        frame.render_widget(ctx_para, chunks[1]);
    }

    let q = Paragraph::new(Line::from(vec![
        Span::styled("orc: ", Style::default().fg(Color::Yellow)),
        Span::styled(question, Style::default().fg(Color::White)),
    ]))
    .wrap(Wrap { trim: false });
    frame.render_widget(q, chunks[2]);

    render_input_box(frame, chunks[4], buffer);

    let hints = hint_line(&[("enter", "submit"), ("esc", "defer")]);
    frame.render_widget(Paragraph::new(hints), hint_area);
}

// ---------------------------------------------------------------------------
// Confirm kill
// ---------------------------------------------------------------------------

fn render_confirm_kill(frame: &mut Frame, area: Rect, name: &str) {
    let rect = centered_fixed(50, 7, area);
    frame.render_widget(Clear, rect);

    let block = modal_block("confirm kill");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let (body, hint_area) = split_body_hint(inner);

    let msg = format!("kill worker \"{}\"?", name);
    let para = Paragraph::new(msg)
        .style(Style::default().fg(Color::Red))
        .wrap(Wrap { trim: false });
    frame.render_widget(para, body);

    let hints = hint_line(&[("y", "confirm"), ("esc/n", "cancel")]);
    frame.render_widget(Paragraph::new(hints), hint_area);
}

// ---------------------------------------------------------------------------
// Confirm quit
// ---------------------------------------------------------------------------

fn render_confirm_quit(frame: &mut Frame, area: Rect) {
    let rect = centered_fixed(44, 7, area);
    frame.render_widget(Clear, rect);

    let block = modal_block("confirm quit");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let (body, hint_area) = split_body_hint(inner);

    let para = Paragraph::new("quit orc? all workers will be terminated.")
        .style(Style::default().fg(Color::Red))
        .wrap(Wrap { trim: false });
    frame.render_widget(para, body);

    let hints = hint_line(&[("y", "confirm"), ("esc/n", "cancel")]);
    frame.render_widget(Paragraph::new(hints), hint_area);
}

// ---------------------------------------------------------------------------
// Help overlay
// ---------------------------------------------------------------------------

fn render_help(frame: &mut Frame, area: Rect) {
    let rect = centered_rect(60, 60, area);
    frame.render_widget(Clear, rect);

    let block = modal_block("keybindings");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let bindings: &[(&str, &str)] = &[
        ("tab / shift+tab", "cycle tabs"),
        ("1-9", "jump to tab"),
        ("n", "new task"),
        ("k", "kill focused worker"),
        ("q", "quit"),
        ("?", "toggle help"),
        ("j / ↓", "scroll down"),
        ("k / ↑", "scroll up"),
        ("g / G", "top / bottom"),
        ("enter", "send chat input"),
        ("esc", "close modal / deselect"),
    ];

    let lines: Vec<Line> = bindings
        .iter()
        .map(|(key, desc)| {
            Line::from(vec![
                Span::styled(
                    format!("{:<20}", key),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(*desc),
            ])
        })
        .collect();

    let para = Paragraph::new(lines)
        .style(Style::default().fg(Color::White))
        .wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn test_area() -> Rect {
        Rect::new(0, 0, 120, 40)
    }

    fn render_to_string(modal: &Modal) -> String {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_modal(frame, test_area(), modal);
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut output = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                output.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
            }
            output.push('\n');
        }
        output
    }

    #[test]
    fn centered_rect_produces_inner_area() {
        let area = Rect::new(0, 0, 100, 50);
        let c = centered_rect(50, 50, area);
        // Should be roughly centered
        assert!(c.x > 0);
        assert!(c.y > 0);
        assert!(c.x + c.width <= area.width);
        assert!(c.y + c.height <= area.height);
    }

    #[test]
    fn centered_fixed_clamps_to_area() {
        let area = Rect::new(0, 0, 30, 10);
        let c = centered_fixed(200, 200, area);
        assert!(c.width <= area.width);
        assert!(c.height <= area.height);
    }

    #[test]
    fn centered_fixed_centers_correctly() {
        let area = Rect::new(0, 0, 100, 50);
        let c = centered_fixed(20, 10, area);
        assert_eq!(c.width, 20);
        assert_eq!(c.height, 10);
        assert_eq!(c.x, 40);
        assert_eq!(c.y, 20);
    }

    #[test]
    fn new_task_modal_renders() {
        let modal = Modal::NewTask {
            target: TabId::Orc,
            buffer: "add OAuth".into(),
        };
        let output = render_to_string(&modal);
        assert!(output.contains("speak to orc"));
        assert!(output.contains("describe the task"));
        assert!(output.contains("add OAuth"));
        assert!(output.contains("enter"));
        assert!(output.contains("send"));
        assert!(output.contains("cancel"));
    }

    #[test]
    fn ask_user_modal_renders() {
        let modal = Modal::AskUser {
            session_id: "explore-agents".into(),
            question_id: "q1".into(),
            question: "install xero-node-sdk?".into(),
            context: Some("npm audit shows no vulns".into()),
            buffer: String::new(),
        };
        let output = render_to_string(&modal);
        assert!(output.contains("orc needs your input"));
        assert!(output.contains("explore-agents"));
        assert!(output.contains("install xero-node-sdk?"));
        assert!(output.contains("defer"));
    }

    #[test]
    fn ask_user_modal_no_context() {
        let modal = Modal::AskUser {
            session_id: "s1".into(),
            question_id: "q2".into(),
            question: "proceed?".into(),
            context: None,
            buffer: "yes".into(),
        };
        let output = render_to_string(&modal);
        assert!(output.contains("proceed?"));
        assert!(output.contains("yes"));
    }

    #[test]
    fn confirm_kill_modal_renders() {
        let modal = Modal::ConfirmKill {
            session_id: "abc".into(),
            name: "worker-1".into(),
        };
        let output = render_to_string(&modal);
        assert!(output.contains("confirm kill"));
        assert!(output.contains("worker-1"));
        assert!(output.contains("y"));
    }

    #[test]
    fn confirm_quit_modal_renders() {
        let output = render_to_string(&Modal::ConfirmQuit);
        assert!(output.contains("confirm quit"));
        assert!(output.contains("quit orc?"));
    }

    #[test]
    fn help_modal_renders() {
        let output = render_to_string(&Modal::Help);
        assert!(output.contains("keybindings"));
        assert!(output.contains("cycle tabs"));
        assert!(output.contains("new task"));
        assert!(output.contains("scroll down"));
    }

    #[test]
    fn small_area_does_not_panic() {
        // Ensure rendering into a tiny terminal doesn't crash.
        let backend = TestBackend::new(10, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let small = Rect::new(0, 0, 10, 5);
        for modal in &[
            Modal::NewTask { target: TabId::Orc, buffer: "x".into() },
            Modal::ConfirmQuit,
            Modal::Help,
        ] {
            terminal
                .draw(|frame| {
                    render_modal(frame, small, modal);
                })
                .unwrap();
        }
    }
}
