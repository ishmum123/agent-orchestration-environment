use crate::agent::OutputEntry;
use crate::app::{App, AppMode};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

pub fn render(f: &mut Frame, app: &App) {
    match &app.mode {
        AppMode::Help => {
            render_dashboard(f, app);
            render_help_overlay(f);
        }
        AppMode::Confirm { message, .. } => {
            render_dashboard(f, app);
            render_confirm_overlay(f, message);
        }
        AppMode::Status => render_status(f, app),
        AppMode::AgentDetail { agent_idx, scroll } => {
            render_agent_detail(f, app, *agent_idx, *scroll);
        }
        _ => render_dashboard(f, app),
    }
}

fn input_height(app: &App, width: u16) -> u16 {
    if !matches!(app.mode, AppMode::Input { .. }) {
        return 3;
    }
    let line_count = app.input_buf.lines().count().max(1) as u16;
    let prompt_len = 3u16;
    let content_width = (width as usize).saturating_sub(prompt_len as usize + 1);
    let wrap_lines = if content_width > 0 {
        app.input_buf.lines()
            .map(|l| ((l.len() / content_width) as u16).max(1))
            .sum::<u16>()
            .max(1)
    } else {
        line_count
    };
    (1 + wrap_lines).min(10).max(2)
}

fn render_dashboard(f: &mut Frame, app: &App) {
    let bottom_h = input_height(app, f.area().width);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(5),
            Constraint::Length(bottom_h),
        ])
        .split(f.area());

    render_header(f, app, chunks[0]);

    if !app.agents.is_empty() {
        let main = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(30),
                Constraint::Length(24),
            ])
            .split(chunks[1]);

        render_orc_output(f, app, main[0]);
        render_agent_sidebar(f, app, main[1]);
    } else {
        render_orc_output(f, app, chunks[1]);
    }

    render_bottom(f, app, chunks[2]);
}

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let working = app.agents.iter()
        .filter(|a| matches!(a.state, crate::agent::AgentState::Working))
        .count();
    let waiting = app.agents.iter()
        .filter(|a| matches!(a.state, crate::agent::AgentState::WaitingForUser))
        .count();
    let errors = app.agents.iter()
        .filter(|a| matches!(a.state, crate::agent::AgentState::Error))
        .count();

    let mut parts = vec![
        Span::styled("orc", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
    ];

    if !app.agents.is_empty() {
        parts.push(Span::styled(
            format!("  {} agents", app.agents.len()),
            Style::default().fg(Color::DarkGray),
        ));
    }

    if working > 0 {
        parts.push(Span::styled(
            format!("  \u{25cf} {}", working),
            Style::default().fg(Color::Green),
        ));
    }
    if waiting > 0 {
        parts.push(Span::styled(
            format!("  \u{25cb} {}", waiting),
            Style::default().fg(Color::Yellow),
        ));
    }
    if errors > 0 {
        parts.push(Span::styled(
            format!("  \u{2717} {}", errors),
            Style::default().fg(Color::Red),
        ));
    }

    let total_cost: f64 = app.agents.iter().map(|a| a.cost_usd).sum();
    if total_cost > 0.001 {
        parts.push(Span::styled(
            format!("  ${:.2}", total_cost),
            Style::default().fg(Color::DarkGray),
        ));
    }

    let header = Paragraph::new(Line::from(parts))
        .block(Block::default().borders(Borders::BOTTOM).border_style(Style::default().fg(Color::DarkGray)));
    f.render_widget(header, area);
}

fn render_orc_output(f: &mut Frame, app: &App, area: Rect) {
    if app.orc_output.is_empty() {
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                "  waiting for orc to start...",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        let msg = Paragraph::new(lines);
        f.render_widget(msg, area);
        return;
    }

    let max_entries = (area.height as usize) + app.scroll_offset;
    let end = app.orc_output.len().saturating_sub(app.scroll_offset);
    let start = end.saturating_sub(max_entries);
    let visible = &app.orc_output[start..end];

    let lines: Vec<Line> = visible.iter()
        .flat_map(|entry| style_output_entry(entry))
        .collect();

    let output = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(output, area);
}

fn render_agent_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app.agents.iter()
        .enumerate()
        .map(|(i, agent)| {
            let is_selected = i == app.selected;
            let marker = if is_selected { "\u{25b8}" } else { " " };

            let name_style = if is_selected {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };

            let spans = vec![
                Span::styled(
                    format!("{} ", marker),
                    if is_selected { Style::default().fg(Color::White) }
                    else { Style::default().fg(Color::DarkGray) },
                ),
                Span::styled(agent.state.icon(), Style::default().fg(agent.state.color())),
                Span::raw(" "),
                Span::styled(truncate(&agent.name, 12), name_style),
                Span::styled(
                    format!(" {:>3}", agent.elapsed_display()),
                    Style::default().fg(Color::DarkGray),
                ),
            ];

            ListItem::new(Line::from(spans))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(Color::DarkGray));

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn render_bottom(f: &mut Frame, app: &App, area: Rect) {
    match &app.mode {
        AppMode::Input { prompt_label, .. } => {
            let border = Paragraph::new("")
                .block(Block::default().borders(Borders::TOP).border_style(Style::default().fg(Color::DarkGray)));
            f.render_widget(border, Rect { height: 1, ..area });

            let text_area = Rect {
                y: area.y + 1,
                height: area.height.saturating_sub(1),
                ..area
            };

            let buf_lines: Vec<&str> = if app.input_buf.is_empty() {
                vec![""]
            } else {
                app.input_buf.lines().collect()
            };

            let mut display_lines: Vec<Line> = Vec::new();
            for (i, line) in buf_lines.iter().enumerate() {
                let prefix = if i == 0 {
                    format!(" {}", prompt_label)
                } else {
                    " .. ".to_string()
                };
                let is_last = i == buf_lines.len() - 1;
                let text = if is_last {
                    format!("{}{}\u{2588}", prefix, line)
                } else {
                    format!("{}{}", prefix, line)
                };
                display_lines.push(Line::from(text));
            }

            let input = Paragraph::new(display_lines)
                .style(Style::default().fg(Color::Yellow));
            f.render_widget(input, text_area);
        }
        _ => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1)])
                .split(area);

            let border = Paragraph::new("")
                .block(Block::default().borders(Borders::TOP).border_style(Style::default().fg(Color::DarkGray)));
            f.render_widget(border, chunks[0]);

            let has_agents = !app.agents.is_empty();
            let mut hints = vec![
                Span::styled(" esc", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(" chat  ", Style::default().fg(Color::DarkGray)),
                Span::styled("n", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled("ew  ", Style::default().fg(Color::DarkGray)),
            ];

            if has_agents {
                hints.extend_from_slice(&[
                    Span::styled("t", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    Span::styled("ell  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("/", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    Span::styled("send  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("e", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    Span::styled("dit  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("x", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    Span::styled("kill  ", Style::default().fg(Color::DarkGray)),
                ]);
            }

            hints.extend_from_slice(&[
                Span::styled("s", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled("tatus  ", Style::default().fg(Color::DarkGray)),
                Span::styled("?", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled("help  ", Style::default().fg(Color::DarkGray)),
                Span::styled("q", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled("uit", Style::default().fg(Color::DarkGray)),
            ]);

            let hints_widget = Paragraph::new(Line::from(hints));
            f.render_widget(hints_widget, chunks[1]);

            let status_text = app.status_display();
            if !status_text.is_empty() {
                let status = Paragraph::new(format!(" {}", status_text))
                    .style(Style::default().fg(Color::DarkGray));
                f.render_widget(status, chunks[2]);
            }
        }
    }
}

fn render_status(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(f.area());

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " status",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {} agents", app.agents.len()),
            Style::default().fg(Color::DarkGray),
        ),
    ]))
    .block(Block::default().borders(Borders::BOTTOM).border_style(Style::default().fg(Color::DarkGray)));
    f.render_widget(header, chunks[0]);

    if app.agents.is_empty() {
        let msg = Paragraph::new(Line::styled(
            " no agents",
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(msg, chunks[1]);
    } else {
        let items: Vec<ListItem> = app.agents.iter()
            .enumerate()
            .map(|(i, agent)| {
                let is_selected = i == app.status_selected;
                let marker = if is_selected { "\u{25b8}" } else { " " };
                let name_style = if is_selected {
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };

                let mut lines = vec![
                    Line::from(vec![
                        Span::styled(
                            format!(" {} ", marker),
                            if is_selected { Style::default().fg(Color::White) }
                            else { Style::default().fg(Color::DarkGray) },
                        ),
                        Span::styled(format!("{} ", agent.state.icon()), Style::default().fg(agent.state.color())),
                        Span::styled(&agent.name, name_style),
                        Span::styled(format!("  [{}]", agent.state.label()), Style::default().fg(agent.state.color())),
                        Span::styled(format!("  {}", agent.elapsed_display()), Style::default().fg(Color::DarkGray)),
                        Span::styled(format!("  ${:.2}", agent.cost_usd), Style::default().fg(Color::DarkGray)),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            format!("     {}", truncate(&agent.task_description, 60)),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]),
                ];

                // Show recent output entries
                for entry in agent.output.recent(3) {
                    let (prefix, text, color) = match entry {
                        OutputEntry::Text(t) => ("\u{2502} ", truncate(t, 70), Color::Gray),
                        OutputEntry::ToolUse { name, .. } => ("\u{2502} ", format!("[{}]", name), Color::Cyan),
                        OutputEntry::Result { text, is_error } => {
                            ("\u{2502} ", truncate(text, 70), if *is_error { Color::Red } else { Color::Green })
                        }
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("     {}{}", prefix, text), Style::default().fg(color)),
                    ]));
                }

                lines.push(Line::from(""));
                ListItem::new(lines)
            })
            .collect();

        let list = List::new(items);
        f.render_widget(list, chunks[1]);
    }

    let hint = Paragraph::new(Line::from(vec![
        Span::styled(" j/k", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(" navigate  ", Style::default().fg(Color::DarkGray)),
        Span::styled("enter", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(" full output  ", Style::default().fg(Color::DarkGray)),
        Span::styled("esc", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(" back", Style::default().fg(Color::DarkGray)),
    ]));
    f.render_widget(hint, chunks[2]);
}

fn agent_input_height(app: &App) -> u16 {
    let buf = &app.agent_input_buf;
    if buf.is_empty() {
        return 2;
    }
    let line_count = buf.lines().count().max(1) as u16;
    (1 + line_count).min(10).max(2)
}

fn render_agent_detail(f: &mut Frame, app: &App, agent_idx: usize, scroll: usize) {
    let agent = match app.agents.get(agent_idx) {
        Some(a) => a,
        None => return,
    };

    let input_h = agent_input_height(app);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(input_h),
        ])
        .split(f.area());

    // Header
    let header_lines = vec![
        Line::from(vec![
            Span::styled(
                format!(" {} ", agent.name),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("[{}]", agent.state.label()), Style::default().fg(agent.state.color())),
            Span::styled(format!("  {}", agent.elapsed_display()), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("  ${:.2}", agent.cost_usd), Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled(format!(" {}", agent.task_description), Style::default().fg(Color::DarkGray)),
        ]),
    ];

    let header = Paragraph::new(header_lines)
        .block(Block::default().borders(Borders::BOTTOM).border_style(Style::default().fg(Color::DarkGray)));
    f.render_widget(header, chunks[0]);

    // Output
    let max_entries = (chunks[1].height as usize) * 2 + scroll;
    let entries = agent.output.recent(max_entries);
    let all_lines: Vec<Line> = entries.iter()
        .flat_map(|e| style_output_entry(e))
        .collect();

    let visible: Vec<Line> = all_lines.into_iter()
        .rev()
        .take(chunks[1].height as usize + scroll)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .skip(scroll)
        .collect();

    let output = Paragraph::new(visible).wrap(Wrap { trim: false });
    f.render_widget(output, chunks[1]);

    // Input box
    let input_area = chunks[2];
    let border = Paragraph::new("")
        .block(Block::default().borders(Borders::TOP).border_style(Style::default().fg(Color::DarkGray)));
    f.render_widget(border, Rect { height: 1, ..input_area });

    let text_area = Rect {
        y: input_area.y + 1,
        height: input_area.height.saturating_sub(1),
        ..input_area
    };

    let prompt = format!("{}> ", agent.name);
    let buf_lines: Vec<&str> = if app.agent_input_buf.is_empty() {
        vec![""]
    } else {
        app.agent_input_buf.lines().collect()
    };

    let mut display_lines: Vec<Line> = Vec::new();
    for (i, line) in buf_lines.iter().enumerate() {
        let prefix = if i == 0 { format!(" {}", prompt) } else { " .. ".to_string() };
        let is_last = i == buf_lines.len() - 1;
        let text = if is_last {
            format!("{}{}\u{2588}", prefix, line)
        } else {
            format!("{}{}", prefix, line)
        };
        display_lines.push(Line::from(text));
    }

    let input = Paragraph::new(display_lines)
        .style(Style::default().fg(Color::Yellow));
    f.render_widget(input, text_area);
}

fn render_help_overlay(f: &mut Frame) {
    let area = centered_rect(52, 24, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            " keybinds ",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ));

    let help_lines = vec![
        Line::from(""),
        help_line("esc", "back to chat (default mode)"),
        help_line("n", "spawn new agent"),
        help_line("t", "tell agent (via orc)"),
        help_line("/", "send directly to agent"),
        help_line("e", "edit changed files ($EDITOR)"),
        help_line("x", "kill agent"),
        help_line("s", "status overview"),
        Line::from(""),
        help_line("j / k", "navigate agents"),
        help_line("enter", "agent full output"),
        help_line("ctrl+u / ctrl+d", "scroll output"),
        Line::from(""),
        help_line("q / ctrl+c", "quit (kills all)"),
        help_line("?", "this help"),
        Line::from(""),
        Line::from(Span::styled(
            "   press any key to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let help = Paragraph::new(help_lines).block(block);
    f.render_widget(help, area);
}

fn render_confirm_overlay(f: &mut Frame, message: &str) {
    let area = centered_rect(44, 7, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(format!("  {}", message), Style::default().fg(Color::White))),
        Line::from(""),
        Line::from(vec![
            Span::styled("  y", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("es  ", Style::default().fg(Color::DarkGray)),
            Span::styled("n", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::styled("o", Style::default().fg(Color::DarkGray)),
        ]),
    ];

    let confirm = Paragraph::new(lines).block(block);
    f.render_widget(confirm, area);
}

/// Style an OutputEntry into one or more Lines for display.
fn style_output_entry(entry: &OutputEntry) -> Vec<Line<'static>> {
    match entry {
        OutputEntry::Text(text) => {
            text.lines().map(|line| {
                Line::from(Span::styled(
                    format!(" {}", line),
                    Style::default().fg(Color::Rgb(180, 180, 195)),
                ))
            }).collect()
        }
        OutputEntry::ToolUse { name, input } => {
            let summary = truncate(input, 60);
            vec![Line::from(vec![
                Span::styled(
                    format!(" \u{25b8} {} ", name),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(summary, Style::default().fg(Color::DarkGray)),
            ])]
        }
        OutputEntry::Result { text, is_error } => {
            let color = if *is_error { Color::Red } else { Color::Green };
            let icon = if *is_error { "\u{2717}" } else { "\u{2713}" };
            text.lines().map(|line| {
                Line::from(Span::styled(
                    format!(" {} {}", icon, line),
                    Style::default().fg(color),
                ))
            }).collect()
        }
    }
}

fn help_line<'a>(key: &'a str, desc: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("   {:>16}", key),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {}", desc), Style::default().fg(Color::Gray)),
    ])
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max <= 3 {
        s[..max].to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}
