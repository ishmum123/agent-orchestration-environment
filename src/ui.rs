use crate::app::{App, AppMode};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

pub fn render(f: &mut Frame, app: &App) {
    match &app.mode {
        AppMode::Status => render_status(f, app),
        _ => render_dashboard(f, app),
    }
}

fn render_dashboard(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // header
            Constraint::Min(5),    // agent list
            Constraint::Length(3), // status bar / input
        ])
        .split(f.area());

    render_header(f, app, chunks[0]);
    render_agent_list(f, app, chunks[1]);
    render_bottom(f, app, chunks[2]);
}

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let working = app
        .agents
        .iter()
        .filter(|a| matches!(a.state, crate::agent::AgentState::Working))
        .count();
    let waiting = app
        .agents
        .iter()
        .filter(|a| matches!(a.state, crate::agent::AgentState::WaitingForUser))
        .count();

    let header = format!(
        "orchestr8 \u{2500} {} agents \u{2500} {} working \u{2500} {} waiting",
        app.agents.len(),
        working,
        waiting,
    );

    let block = Paragraph::new(header)
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::BOTTOM));

    f.render_widget(block, area);
}

fn render_agent_list(f: &mut Frame, app: &App, area: Rect) {
    if app.agents.is_empty() {
        let msg = Paragraph::new("No agents. Press [n] to spawn one.")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = app
        .agents
        .iter()
        .enumerate()
        .map(|(i, agent)| {
            let selected_marker = if i == app.selected {
                "\u{25b8} "
            } else {
                "  "
            };
            let state_label = agent.state.label();
            let state_color = agent.state.color();

            let context_bar = "\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2593}\u{2593}";

            let line = Line::from(vec![
                Span::styled(selected_marker, Style::default().fg(Color::Green)),
                Span::styled(
                    format!("{:<16}", agent.name),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("[{}]", state_label),
                    Style::default().fg(state_color),
                ),
                Span::raw("   "),
                Span::styled(
                    truncate(&agent.task_description, 40),
                    Style::default().fg(Color::Gray),
                ),
                Span::raw("   "),
                Span::styled(context_bar, Style::default().fg(Color::DarkGray)),
            ]);

            ListItem::new(line)
        })
        .collect();

    let list = List::new(items).block(Block::default().borders(Borders::NONE));

    f.render_widget(list, area);
}

fn render_bottom(f: &mut Frame, app: &App, area: Rect) {
    match &app.mode {
        AppMode::Input { prompt_label, .. } => {
            let input_text = format!("{}{}\u{2588}", prompt_label, app.input_buf);
            let input = Paragraph::new(input_text)
                .style(Style::default().fg(Color::Yellow))
                .block(Block::default().borders(Borders::TOP));
            f.render_widget(input, area);
        }
        _ => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Length(2)])
                .split(area);

            let hints = Line::from(vec![
                Span::styled("[a]", Style::default().fg(Color::Green)),
                Span::raw("ttach "),
                Span::styled("[t]", Style::default().fg(Color::Green)),
                Span::raw("ell "),
                Span::styled("[d]", Style::default().fg(Color::Green)),
                Span::raw("iff "),
                Span::styled("[s]", Style::default().fg(Color::Green)),
                Span::raw("tatus "),
                Span::styled("[n]", Style::default().fg(Color::Green)),
                Span::raw("ew "),
                Span::styled("[q]", Style::default().fg(Color::Green)),
                Span::raw("uit"),
            ]);
            let hints_widget =
                Paragraph::new(hints).block(Block::default().borders(Borders::TOP));
            f.render_widget(hints_widget, chunks[0]);

            if !app.status_line.is_empty() {
                let status = Paragraph::new(format!("last: {}", app.status_line))
                    .style(Style::default().fg(Color::DarkGray));
                f.render_widget(status, chunks[1]);
            }
        }
    }
}

fn render_status(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(f.area());

    let header = Paragraph::new("Agent Status Summary")
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::BOTTOM));
    f.render_widget(header, chunks[0]);

    if app.agents.is_empty() {
        let msg = Paragraph::new("No agents spawned yet.")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(msg, chunks[1]);
    } else {
        let items: Vec<ListItem> = app
            .agents
            .iter()
            .map(|agent| {
                let summary = agent
                    .last_output
                    .lines()
                    .rev()
                    .filter(|l| !l.trim().is_empty())
                    .take(3)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join(" | ");
                let summary = truncate(&summary, 70);

                let lines = vec![
                    Line::from(vec![
                        Span::styled(
                            format!("{}: ", agent.name),
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("[{}]", agent.state.label()),
                            Style::default().fg(agent.state.color()),
                        ),
                    ]),
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(summary, Style::default().fg(Color::Gray)),
                    ]),
                    Line::from(""),
                ];

                ListItem::new(lines)
            })
            .collect();

        let list = List::new(items).block(Block::default().borders(Borders::NONE));
        f.render_widget(list, chunks[1]);
    }

    let hint = Paragraph::new("[Esc] back to dashboard")
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(hint, chunks[2]);
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}
