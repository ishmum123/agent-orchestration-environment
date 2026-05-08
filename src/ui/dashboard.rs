// Dashboard view: orchestrator tab with chat pane and optional task graph.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, ChatRole, TabId};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn render_dashboard(frame: &mut Frame, area: Rect, app: &App) {
    if app.show_graph {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area);
        render_chat(frame, chunks[0], app);
        render_graph(frame, chunks[1], app);
    } else {
        render_chat(frame, area, app);
    }
}

// ---------------------------------------------------------------------------
// Chat pane
// ---------------------------------------------------------------------------

fn role_prefix(role: &ChatRole) -> (&'static str, Color) {
    match role {
        ChatRole::User => ("you: ", Color::Cyan),
        ChatRole::Orc => ("orc: ", Color::Yellow),
        ChatRole::System => ("sys: ", Color::DarkGray),
    }
}

fn render_chat(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" chat ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 2 || inner.height < 1 {
        return;
    }

    let lines = build_chat_lines(&app.orc_chat, inner.width);

    let visible_height = inner.height as usize;
    let scroll = app.scroll_pos(TabId::Orc);
    let total = lines.len();

    // Clamp scroll so we don't go past content.
    let max_scroll = total.saturating_sub(visible_height);
    let offset = scroll.min(max_scroll);

    let para = Paragraph::new(lines)
        .scroll((offset as u16, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(para, inner);
}

/// Convert chat messages into styled Lines, wrapping is handled by Paragraph.
fn build_chat_lines(messages: &[crate::app::ChatMessage], _width: u16) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    for msg in messages {
        let (prefix, color) = role_prefix(&msg.role);
        let text_lines: Vec<&str> = msg.text.lines().collect();

        if text_lines.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(color).add_modifier(Modifier::BOLD)),
            ]));
        } else {
            // First line gets the role prefix.
            lines.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(color).add_modifier(Modifier::BOLD)),
                Span::styled(text_lines[0].to_string(), Style::default().fg(color)),
            ]));
            // Continuation lines indented to align past the prefix.
            let indent = " ".repeat(prefix.len());
            for &continuation in &text_lines[1..] {
                lines.push(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(continuation.to_string(), Style::default().fg(color)),
                ]));
            }
        }

        // Blank separator between messages.
        lines.push(Line::from(""));
    }

    lines
}

// ---------------------------------------------------------------------------
// Task graph pane (placeholder — full DAG rendering is Phase 4+)
// ---------------------------------------------------------------------------

fn render_graph(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" task graph ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 2 || inner.height < 1 {
        return;
    }

    if app.sessions.is_empty() {
        let empty = Paragraph::new("  no sessions")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(empty, inner);
        return;
    }

    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .map(|sv| {
            let (badge, badge_color) = App::state_badge(&sv.session.state);
            let elapsed = App::elapsed_str(&sv.session);
            let line = Line::from(vec![
                Span::styled(
                    format!(" {badge} "),
                    Style::default().fg(badge_color),
                ),
                Span::styled(
                    sv.session.name.clone(),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!(" ({}) ", sv.session.model),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    elapsed,
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items);
    frame.render_widget(list, inner);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, ChatMessage, ChatRole};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::Instant;

    fn make_app_with_chat() -> App {
        let mut app = App::new(".");
        app.orc_chat.push(ChatMessage {
            role: ChatRole::User,
            text: "explore the repo".to_string(),
            timestamp: Instant::now(),
        });
        app.orc_chat.push(ChatMessage {
            role: ChatRole::Orc,
            text: "planned 3 sessions:\n  1. explore\n  2. migrate".to_string(),
            timestamp: Instant::now(),
        });
        app.orc_chat.push(ChatMessage {
            role: ChatRole::System,
            text: "session started".to_string(),
            timestamp: Instant::now(),
        });
        app
    }

    #[test]
    fn build_chat_lines_produces_correct_count() {
        let app = make_app_with_chat();
        let lines = build_chat_lines(&app.orc_chat, 80);
        // Message 1: 1 line + blank = 2
        // Message 2: 3 lines + blank = 4
        // Message 3: 1 line + blank = 2
        assert_eq!(lines.len(), 8);
    }

    #[test]
    fn first_line_has_role_prefix() {
        let app = make_app_with_chat();
        let lines = build_chat_lines(&app.orc_chat, 80);
        let first_spans = &lines[0].spans;
        assert_eq!(first_spans[0].content, "you: ");
    }

    #[test]
    fn continuation_lines_are_indented() {
        let app = make_app_with_chat();
        let lines = build_chat_lines(&app.orc_chat, 80);
        // Orc message starts at index 2 (after user msg + blank).
        // Index 3 is first continuation of the orc message.
        let cont = &lines[3].spans;
        // First span should be whitespace indent matching "orc: " (5 chars).
        assert_eq!(cont[0].content, "     ");
    }

    #[test]
    fn render_dashboard_no_panic_empty() {
        let app = App::new(".");
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_dashboard(frame, area, &app);
            })
            .unwrap();
    }

    #[test]
    fn render_dashboard_with_graph_no_panic() {
        let mut app = make_app_with_chat();
        app.show_graph = true;
        // Add a session for the graph pane.
        use crate::session::{Session, SessionMode, SessionState};
        app.add_session(Session {
            id: "s1".to_string(),
            name: "explore".to_string(),
            task: "explore repo".to_string(),
            worktree_path: "/tmp/wt".to_string(),
            branch: "explore".to_string(),
            base_commit: "abc".to_string(),
            tmux_session: "orc-explore".to_string(),
            state: SessionState::Running,
            mode: SessionMode::Watch,
            model: "sonnet".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ended_at: None,
        });

        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_dashboard(frame, area, &app);
            })
            .unwrap();
    }

    #[test]
    fn chat_pane_fills_full_width_when_graph_hidden() {
        // With show_graph=false, render_dashboard calls render_chat with the full area.
        // We verify by checking no panic and that the block title is present in buffer.
        let app = make_app_with_chat();
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_dashboard(frame, area, &app);
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let content: String = (0..buf.area.width)
            .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
            .collect();
        assert!(content.contains("chat"), "expected 'chat' in top border");
    }

    #[test]
    fn graph_pane_visible_when_toggled() {
        let mut app = App::new(".");
        app.show_graph = true;

        let backend = TestBackend::new(100, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_dashboard(frame, area, &app);
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let content: String = (0..buf.area.width)
            .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
            .collect();
        assert!(content.contains("task graph"), "expected 'task graph' in top border");
    }

    #[test]
    fn scroll_offset_clamps_to_content() {
        let mut app = make_app_with_chat();
        // Scroll way past content.
        app.scroll_down(TabId::Orc, 1000);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_dashboard(frame, area, &app);
            })
            .unwrap();
        // No panic is the assertion.
    }

    #[test]
    fn role_prefixes_correct() {
        assert_eq!(role_prefix(&ChatRole::User).0, "you: ");
        assert_eq!(role_prefix(&ChatRole::Orc).0, "orc: ");
        assert_eq!(role_prefix(&ChatRole::System).0, "sys: ");
    }
}
