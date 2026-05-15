// Modal overlays: confirm dialogs, help screens, task input, permission prompts.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::app::Modal;
use crate::input_attachments::AttachmentSet;

// Max chips rendered inline; overflow becomes "+N more".
const CHIP_VISIBLE_MAX: usize = 6;

fn attachment_chip_line(set: &AttachmentSet) -> Line<'static> {
    if set.is_empty() {
        return Line::from("");
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    let total = set.len();
    let mut shown = 0usize;
    for a in set.iter().take(CHIP_VISIBLE_MAX) {
        spans.push(Span::styled(
            format!(" [{} ✕] ", truncate_name(&a.display_name, 24)),
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
    Line::from(spans)
}

fn truncate_name(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn render_modal(frame: &mut Frame, area: Rect, modal: &Modal) {
    match modal {
        Modal::AskUser {
            session_id,
            question_id: _,
            question,
            context: _,
            buffer,
            hidden,
            attachments,
        } => {
            if *hidden {
                return;
            }
            render_ask_user(frame, area, session_id, question, None, buffer, attachments);
        }
        Modal::Comment { file, line, buffer, .. } => {
            render_comment(frame, area, file, *line, buffer)
        }
        Modal::ConfirmKill { session_id: _, name } => render_confirm_kill(frame, area, name),
        Modal::ConfirmQuit => render_confirm_quit(frame, area),
        Modal::ConfirmMerge { name, branch, target, .. } => {
            render_confirm_merge(frame, area, name, branch, target)
        }
        Modal::ResumeRunning { names } => render_resume_running(frame, area, names),
        Modal::ConfirmPush { branch, target } => render_confirm_push(frame, area, branch, target),
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
    render_input_box_with_placeholder(frame, area, buffer, "")
}

fn render_input_box_with_placeholder(
    frame: &mut Frame,
    area: Rect,
    buffer: &str,
    placeholder: &str,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if buffer.is_empty() && !placeholder.is_empty() {
        // Placeholder line + cursor on column 0.
        let line = Line::from(vec![
            Span::styled("_", Style::default().fg(Color::White)),
            Span::styled(format!(" {placeholder}"), DIM.add_modifier(Modifier::ITALIC)),
        ]);
        let para = Paragraph::new(line).wrap(Wrap { trim: false });
        frame.render_widget(para, inner);
        return;
    }

    let text = format!("{buffer}_");
    let paragraph = Paragraph::new(text)
        .style(Style::default().fg(Color::White))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

/// Split `area` into an optional chip row (1 line, shown only when
/// attachments are non-empty) plus the input box below.
fn render_input_with_chips(
    frame: &mut Frame,
    area: Rect,
    buffer: &str,
    placeholder: &str,
    attachments: &AttachmentSet,
) {
    if attachments.is_empty() {
        render_input_box_with_placeholder(frame, area, buffer, placeholder);
        return;
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3)])
        .split(area);
    frame.render_widget(Paragraph::new(attachment_chip_line(attachments)), chunks[0]);
    render_input_box_with_placeholder(frame, chunks[1], buffer, placeholder);
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
    attachments: &AttachmentSet,
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

    render_input_with_chips(frame, chunks[4], buffer, "", attachments);

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
// Confirm merge (after approving a review)
// ---------------------------------------------------------------------------

fn render_confirm_merge(frame: &mut Frame, area: Rect, name: &str, branch: &str, target: &str) {
    let rect = centered_fixed(60, 9, area);
    frame.render_widget(Clear, rect);

    let block = modal_block("merge approved work");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let (body, hint_area) = split_body_hint(inner);

    let lines = vec![
        Line::from(vec![
            Span::raw("merge worker "),
            Span::styled(name.to_string(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw(" into "),
            Span::styled(target.to_string(), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw("?"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("branch: ", DIM),
            Span::styled(branch.to_string(), Style::default().fg(Color::Gray)),
        ]),
    ];
    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, body);

    let hints = hint_line(&[("enter", "merge"), ("esc", "skip")]);
    frame.render_widget(Paragraph::new(hints), hint_area);
}

// ---------------------------------------------------------------------------
// Confirm push (after a successful merge, when origin remote exists)
// ---------------------------------------------------------------------------

fn render_confirm_push(frame: &mut Frame, area: Rect, branch: &str, target: &str) {
    let rect = centered_fixed(60, 9, area);
    frame.render_widget(Clear, rect);

    let block = modal_block("push to remote?");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let (body, hint_area) = split_body_hint(inner);

    let lines = vec![
        Line::from(vec![
            Span::raw("merged "),
            Span::styled(branch.to_string(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw(" into "),
            Span::styled(target.to_string(), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw("."),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("push "),
            Span::styled(target.to_string(), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw(" to "),
            Span::styled("origin", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            Span::raw("?"),
        ]),
    ];
    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, body);

    let hints = hint_line(&[("enter", "push"), ("esc", "skip")]);
    frame.render_widget(Paragraph::new(hints), hint_area);
}

// ---------------------------------------------------------------------------
// Resume-Running (offered on startup when prior workers were active)
// ---------------------------------------------------------------------------

fn render_resume_running(frame: &mut Frame, area: Rect, names: &[String]) {
    let height = (6 + names.len()).min(20) as u16;
    let rect = centered_fixed(60, height, area);
    frame.render_widget(Clear, rect);

    let block = modal_block("resume previous workers?");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let (body, hint_area) = split_body_hint(inner);

    let mut lines: Vec<Line> = vec![
        Line::from(vec![Span::raw(
            "the following workers were running last time orc was open:",
        )]),
        Line::from(""),
    ];
    for n in names {
        lines.push(Line::from(vec![
            Span::raw("  · "),
            Span::styled(
                n.clone(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "resume them? (their claude processes did not survive)",
        DIM,
    )));

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, body);

    let hints = hint_line(&[("y / enter", "resume all"), ("n / esc", "leave as-is")]);
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

    // Sectioned for scannability. Each section starts with a dim header.
    let sections: &[(&str, &[(&str, &str)])] = &[
        (
            "navigate",
            &[
                ("tab / shift+tab", "cycle tabs"),
                ("1-9", "jump to tab"),
                ("!", "jump to next attention (review/failed/blocked)"),
                ("j / ↓", "scroll down"),
                ("↑ / pgup", "scroll up"),
                ("gg / home", "jump to top"),
                ("G / end", "jump to bottom (auto-follow)"),
            ],
        ),
        (
            "act on focused tab",
            &[
                ("c", "chat (new task on orc / message a worker)"),
                ("⏎", "control mode toggle (worker tab)"),
                ("r", "open review (awaiting review)"),
                ("x", "kill focused worker"),
                ("R", "restart failed worker"),
                ("Ctrl+c", "interrupt"),
                ("n", "fullscreen claude (scratch session, project cwd)"),
                ("?", "scratch claude overlay (sealed sidekick, sonnet)"),
            ],
        ),
        (
            "modal / global",
            &[
                ("enter", "send chat input"),
                ("esc", "close modal / deselect"),
                ("h", "toggle help"),
                ("q", "quit"),
            ],
        ),
    ];

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, (heading, items)) in sections.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            heading.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        for (key, desc) in *items {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{:<18}", key),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(desc.to_string(), Style::default().fg(Color::Gray)),
            ]));
        }
    }

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
    fn ask_user_modal_renders() {
        let modal = Modal::AskUser {
            session_id: "explore-agents".into(),
            question_id: "q1".into(),
            question: "install xero-node-sdk?".into(),
            context: Some("npm audit shows no vulns".into()),
            buffer: String::new(),
            hidden: false,
            attachments: AttachmentSet::new(),
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
            hidden: false,
            attachments: AttachmentSet::new(),
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
