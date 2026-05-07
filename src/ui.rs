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
        return 3; // hints + status
    }
    let line_count = app.input_buf.lines().count().max(1) as u16;
    // Also account for wrapping within the available width
    let prompt_len = 3u16; // "> " + cursor
    let content_width = (width as usize).saturating_sub(prompt_len as usize + 1);
    let wrap_lines = if content_width > 0 {
        app.input_buf.lines()
            .map(|l| ((l.len() / content_width) as u16).max(1))
            .sum::<u16>()
            .max(1)
    } else {
        line_count
    };
    // 1 for border + lines for content, capped at 10
    (1 + wrap_lines).min(10).max(2)
}

fn render_dashboard(f: &mut Frame, app: &App) {
    let bottom_h = input_height(app, f.area().width);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),  // header
            Constraint::Min(5),    // main area
            Constraint::Length(bottom_h), // bottom bar (dynamic)
        ])
        .split(f.area());

    render_header(f, app, chunks[0]);

    if !app.agents.is_empty() {
        // Orc conversation (left) + agent sidebar (right)
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
        // Full-width orc conversation
        render_orc_output(f, app, chunks[1]);
    }

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
    let stuck = app
        .agents
        .iter()
        .filter(|a| matches!(a.state, crate::agent::AgentState::Stuck))
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
    if stuck > 0 {
        parts.push(Span::styled(
            format!("  \u{2717} {}", stuck),
            Style::default().fg(Color::Red),
        ));
    }

    let header = Paragraph::new(Line::from(parts))
        .block(Block::default().borders(Borders::BOTTOM).border_style(Style::default().fg(Color::DarkGray)));

    f.render_widget(header, area);
}

fn render_orc_output(f: &mut Frame, app: &App, area: Rect) {
    let output = &app.orc_output;

    if output.trim().is_empty() {
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

    // Claude Code layout: banner at top (~5 lines), status bar at bottom (~3 lines).
    // Strip those by position rather than content matching to avoid false positives.
    let all_lines: Vec<&str> = output.lines().collect();
    let content_lines = strip_claude_chrome(&all_lines);

    let visible_lines: Vec<Line> = content_lines
        .into_iter()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(area.height as usize + app.scroll_offset)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .skip(app.scroll_offset)
        .map(|line| style_orc_line(line))
        .collect();

    let output_widget = Paragraph::new(visible_lines)
        .wrap(Wrap { trim: false });
    f.render_widget(output_widget, area);
}

fn render_agent_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .agents
        .iter()
        .enumerate()
        .map(|(i, agent)| {
            let is_selected = i == app.selected;
            let state_icon = agent.state.icon();
            let state_color = agent.state.color();
            let elapsed = agent.elapsed_display();

            let name_style = if is_selected {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };

            let marker = if is_selected { "\u{25b8}" } else { " " };

            let mut spans = vec![
                Span::styled(
                    format!("{} ", marker),
                    if is_selected {
                        Style::default().fg(Color::White)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                ),
                Span::styled(state_icon, Style::default().fg(state_color)),
                Span::raw(" "),
                Span::styled(
                    truncate(&agent.name, 12),
                    name_style,
                ),
                Span::styled(
                    format!(" {:>3}", elapsed),
                    Style::default().fg(Color::DarkGray),
                ),
            ];

            if agent.locked {
                spans.push(Span::styled(
                    " \u{1f512}",
                    Style::default().fg(Color::Yellow),
                ));
            }

            let line = Line::from(spans);
            ListItem::new(line)
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
            let input_area = Rect {
                y: area.y,
                height: area.height,
                ..area
            };

            let border = Paragraph::new("")
                .block(Block::default().borders(Borders::TOP).border_style(Style::default().fg(Color::DarkGray)));
            f.render_widget(border, Rect { height: 1, ..input_area });

            let text_area = Rect {
                y: area.y + 1,
                height: area.height.saturating_sub(1),
                ..area
            };

            // Build multi-line display
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
        let items: Vec<ListItem> = app
            .agents
            .iter()
            .enumerate()
            .map(|(i, agent)| {
                let is_selected = i == app.status_selected;
                let output_lines: Vec<&str> = agent
                    .last_output
                    .lines()
                    .rev()
                    .filter(|l| !l.trim().is_empty())
                    .take(5)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();

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
                            if is_selected {
                                Style::default().fg(Color::White)
                            } else {
                                Style::default().fg(Color::DarkGray)
                            },
                        ),
                        Span::styled(
                            format!("{} ", agent.state.icon()),
                            Style::default().fg(agent.state.color()),
                        ),
                        Span::styled(
                            &agent.name,
                            name_style,
                        ),
                        Span::styled(
                            format!("  [{}]", agent.state.label()),
                            Style::default().fg(agent.state.color()),
                        ),
                        Span::styled(
                            format!("  {}", agent.elapsed_display()),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            format!("     {}", truncate(&agent.task_description, 60)),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]),
                ];

                for ol in &output_lines {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("     \u{2502} {}", truncate(ol, 70)),
                            Style::default().fg(Color::Gray),
                        ),
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

fn agent_input_height(app: &App, width: u16) -> u16 {
    let buf = &app.agent_input_buf;
    if buf.is_empty() {
        return 2; // border + single line
    }
    let line_count = buf.lines().count().max(1) as u16;
    (1 + line_count).min(10).max(2)
}

fn render_agent_detail(f: &mut Frame, app: &App, agent_idx: usize, scroll: usize) {
    let agent = match app.agents.get(agent_idx) {
        Some(a) => a,
        None => return,
    };

    let input_h = agent_input_height(app, f.area().width);
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
            Span::styled(
                format!("[{}]", agent.state.label()),
                Style::default().fg(agent.state.color()),
            ),
            Span::styled(
                format!("  {}", agent.elapsed_display()),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                format!(" {}", agent.task_description),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ];

    let header = Paragraph::new(header_lines)
        .block(Block::default().borders(Borders::BOTTOM).border_style(Style::default().fg(Color::DarkGray)));
    f.render_widget(header, chunks[0]);

    // Output — strip chrome, style lines
    let all_lines: Vec<&str> = agent.last_output.lines().collect();
    let content = strip_claude_chrome(&all_lines);

    let visible_lines: Vec<Line> = content
        .into_iter()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(chunks[1].height as usize + scroll)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .skip(scroll)
        .map(|line| style_orc_line(line))
        .collect();

    let output = Paragraph::new(visible_lines)
        .wrap(Wrap { trim: false });
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
        let prefix = if i == 0 {
            format!(" {}", prompt)
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
        help_line("ctrl+u / ctrl+d", "scroll orc output"),
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
        Line::from(Span::styled(
            format!("  {}", message),
            Style::default().fg(Color::White),
        )),
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

/// Style a line of orc output based on Claude Code conversation structure
fn style_orc_line(line: &str) -> Line<'static> {
    let trimmed = line.trim();
    let owned = format!(" {}", line);

    // User prompt: ❯
    if trimmed.starts_with('\u{276f}') {
        let content = trimmed.trim_start_matches('\u{276f}').trim_start().to_string();
        return Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(
                "\u{276f} ".to_string(),
                Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                content,
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
        ]);
    }

    // Assistant response marker: ⏺
    if trimmed.starts_with('\u{23fa}') {
        let content = trimmed.trim_start_matches('\u{23fa}').trim_start().to_string();
        return Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(
                "\u{23fa} ".to_string(),
                Style::default().fg(Color::Magenta),
            ),
            Span::styled(
                content,
                Style::default().fg(Color::White),
            ),
        ]);
    }

    // Tool use indicators: ⎿
    if trimmed.starts_with('\u{23bf}') || trimmed.starts_with('\u{2502}') {
        return Line::styled(owned, Style::default().fg(Color::DarkGray));
    }

    // Spinning/status indicators
    if trimmed.starts_with('\u{2736}')  // ✶
        || trimmed.starts_with('\u{25d0}') // ◐
        || trimmed.starts_with('\u{25cf}') // ●
        || trimmed.contains("Harmonizing")
        || trimmed.contains("Pontificating")
        || trimmed.contains("Thinking")
        || trimmed.contains("Running")
    {
        return Line::styled(owned, Style::default().fg(Color::Yellow));
    }

    // Errors
    if trimmed.to_lowercase().contains("error") {
        return Line::styled(owned, Style::default().fg(Color::Red));
    }

    // Success
    if trimmed.contains('\u{2713}') || trimmed.to_lowercase().contains("success")
        || trimmed.to_lowercase().contains("complete")
    {
        return Line::styled(owned, Style::default().fg(Color::Green));
    }

    // Bullets / list items
    if trimmed.starts_with('-') || trimmed.starts_with('*') || trimmed.starts_with('\u{2022}') {
        return Line::styled(owned, Style::default().fg(Color::Rgb(200, 200, 220)));
    }

    // Code/command
    if trimmed.starts_with("```") || trimmed.starts_with('$') {
        return Line::styled(owned, Style::default().fg(Color::Cyan));
    }

    // Default
    Line::styled(owned, Style::default().fg(Color::Rgb(180, 180, 195)))
}

/// Strip Claude Code UI chrome from captured pane output.
/// Uses a hybrid approach: find conversation start by looking for the first real
/// user prompt (not a "Try" suggestion), then filter remaining chrome lines.
fn strip_claude_chrome<'a>(lines: &'a [&'a str]) -> Vec<&'a str> {
    // Find first real content: ❯ not followed by Try/suggestion, or ⏺ response
    let start = lines.iter().position(|l| {
        let t = l.trim();
        (t.starts_with('\u{276f}') && !t.contains("Try \"") && !t.contains("Try \u{201c}"))  // ❯ but not suggestion
            || t.starts_with('\u{23fa}') // ⏺
    }).unwrap_or(lines.len());

    if start >= lines.len() {
        return Vec::new();
    }

    // From start, keep only non-chrome lines
    lines[start..]
        .iter()
        .filter(|l| !is_chrome_line(l))
        .copied()
        .collect()
}

fn is_chrome_line(line: &str) -> bool {
    let l = line.trim();
    l.contains("-- INSERT --")
        || l.contains("-- NORMAL --")
        || l.contains("Model: ")
        || l.contains("Ctx Used:")
        || l.contains("bypass permissions")
        || l.contains("/effort")
        || l.contains("native installer")
        || l.contains("claude install")
        || (l.starts_with('\u{276f}') && (l.contains("Try \"") || l.contains("Try \u{201c}")))
        // Separator lines (only box-drawing chars and spaces)
        || (l.len() > 20 && l.chars().all(|c| "\u{2500}\u{2501}\u{2502}\u{2550}\u{2551} -".contains(c)))
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
