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
        AppMode::AgentDetail { agent_idx, scroll, browsing } => {
            render_agent_detail(f, app, *agent_idx, *scroll, *browsing);
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

    // Render all entries into lines, then show the tail (adjusted by scroll_offset)
    let all_lines: Vec<Line> = app.orc_output.iter()
        .flat_map(|entry| style_output_entry(entry))
        .collect();

    let height = area.height as usize;
    let total = all_lines.len();
    // scroll_offset=0 means show the bottom; higher values scroll up
    let end = total.saturating_sub(app.scroll_offset);
    let start = end.saturating_sub(height);
    let visible: Vec<Line> = all_lines.into_iter().skip(start).take(end - start).collect();

    let output = Paragraph::new(visible).wrap(Wrap { trim: false });
    f.render_widget(output, area);
}

fn render_agent_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let in_agent_panel = matches!(app.mode, AppMode::AgentPanel);
    let items: Vec<ListItem> = app.agents.iter()
        .enumerate()
        .map(|(i, agent)| {
            let is_selected = in_agent_panel && i == app.selected;
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
                if agent.prune_after_orc_prompt.is_some() {
                    Span::styled("\u{25cc}", Style::default().fg(Color::DarkGray)) // ◌ prune candidate
                } else {
                    Span::styled(agent.state.icon(), Style::default().fg(agent.state.color()))
                },
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
            let in_agent_panel = matches!(app.mode, AppMode::AgentPanel);
            let mut hints = vec![
                Span::styled(" esc", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(" chat  ", Style::default().fg(Color::DarkGray)),
                Span::styled("n", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled("ew  ", Style::default().fg(Color::DarkGray)),
            ];

            if has_agents {
                hints.extend_from_slice(&[
                    Span::styled("a", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    Span::styled("gents  ", Style::default().fg(Color::DarkGray)),
                ]);
            }

            if in_agent_panel && has_agents {
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

fn agent_input_height(app: &App) -> u16 {
    let buf = &app.agent_input_buf;
    if buf.is_empty() {
        return 2;
    }
    let line_count = buf.lines().count().max(1) as u16;
    (1 + line_count).min(10).max(2)
}

fn render_agent_detail(f: &mut Frame, app: &App, agent_idx: usize, scroll: usize, browsing: bool) {
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
    let mode_label = if browsing { " BROWSE " } else { " INPUT " };
    let mode_color = if browsing { Color::Cyan } else { Color::Green };
    let header_lines = vec![
        Line::from(vec![
            Span::styled(
                format!(" {} ", agent.name),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("[{}]", agent.state.label()), Style::default().fg(agent.state.color())),
            Span::styled(format!("  {}", agent.elapsed_display()), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("  {}", mode_label), Style::default().fg(Color::Black).bg(mode_color)),
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

    let mut display_lines: Vec<Line> = Vec::new();
    if browsing {
        display_lines.push(Line::from(Span::styled(
            " j/k scroll  Ctrl+U/D page  i or Enter → input  Esc → back",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let prompt = format!("{}> ", agent.name);
        let buf_lines: Vec<&str> = if app.agent_input_buf.is_empty() {
            vec![""]
        } else {
            app.agent_input_buf.lines().collect()
        };

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
        help_line("esc", "dashboard (from chat/agents)"),
        help_line("enter / esc", "chat mode"),
        help_line("j / k", "scroll orc output"),
        help_line("a", "agents panel"),
        help_line("n", "spawn new agent"),
        help_line("e", "edit changed files ($EDITOR)"),
        Line::from(""),
        Line::from(Span::styled("  agents panel:", Style::default().fg(Color::White).add_modifier(Modifier::BOLD))),
        help_line("j / k", "navigate agents"),
        help_line("enter", "open agent detail"),
        help_line("t", "tell agent (via orc)"),
        help_line("/", "send directly to agent"),
        help_line("x", "kill agent"),
        Line::from(""),
        help_line("ctrl+u / ctrl+d", "scroll output"),
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
            let mut lines = Vec::new();
            let mut in_code_block = false;
            for line in text.lines() {
                if line.trim_start().starts_with("```") {
                    in_code_block = !in_code_block;
                    continue;
                }
                if in_code_block {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", line),
                        Style::default().fg(Color::Rgb(140, 170, 140)).add_modifier(Modifier::DIM),
                    )));
                } else if is_table_separator(line) {
                    // Skip |---|---| lines
                    continue;
                } else if is_table_row(line) {
                    lines.push(style_table_row(line));
                } else {
                    lines.push(style_markdown_line(line));
                }
            }
            lines
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
        OutputEntry::UserInput(text) => {
            text.lines().map(|line| {
                Line::from(Span::styled(
                    format!(" \u{25b8} {}", line),
                    Style::default().fg(Color::Yellow),
                ))
            }).collect()
        }
    }
}

/// Parse a single line of markdown into styled spans.
fn style_markdown_line(line: &str) -> Line<'static> {
    // Headers
    if let Some(rest) = line.strip_prefix("### ") {
        return Line::from(Span::styled(
            format!(" {}", rest),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(rest) = line.strip_prefix("## ") {
        return Line::from(Span::styled(
            format!(" {}", rest),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(rest) = line.strip_prefix("# ") {
        return Line::from(Span::styled(
            format!(" {}", rest),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ));
    }

    // List items: keep the bullet/dash
    let (prefix, content) = if line.starts_with("- ") || line.starts_with("* ") {
        (" \u{2022} ".to_string(), &line[2..])
    } else if line.starts_with("  - ") || line.starts_with("  * ") {
        ("   \u{2022} ".to_string(), &line[4..])
    } else {
        (" ".to_string(), line)
    };

    // Parse inline formatting: **bold**, `code`
    let spans = parse_inline_markdown(content);
    let mut result = vec![Span::styled(prefix, Style::default().fg(Color::Rgb(180, 180, 195)))];
    result.extend(spans);
    Line::from(result)
}

/// Parse inline **bold** and `code` spans from text.
fn parse_inline_markdown(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        // Find the next marker
        let bold_pos = remaining.find("**");
        let code_pos = remaining.find('`');

        let next = match (bold_pos, code_pos) {
            (Some(b), Some(c)) => {
                if b <= c { Some(("**", b)) } else { Some(("`", c)) }
            }
            (Some(b), None) => Some(("**", b)),
            (None, Some(c)) => Some(("`", c)),
            (None, None) => None,
        };

        match next {
            Some((marker, pos)) => {
                // Text before the marker
                if pos > 0 {
                    spans.push(Span::styled(
                        remaining[..pos].to_string(),
                        Style::default().fg(Color::Rgb(180, 180, 195)),
                    ));
                }
                let after_open = &remaining[pos + marker.len()..];
                if let Some(end) = after_open.find(marker) {
                    let content = &after_open[..end];
                    let style = if marker == "**" {
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Rgb(140, 170, 140))
                    };
                    spans.push(Span::styled(content.to_string(), style));
                    remaining = &after_open[end + marker.len()..];
                } else {
                    // No closing marker, treat as plain text
                    spans.push(Span::styled(
                        remaining[pos..pos + marker.len()].to_string(),
                        Style::default().fg(Color::Rgb(180, 180, 195)),
                    ));
                    remaining = after_open;
                }
            }
            None => {
                spans.push(Span::styled(
                    remaining.to_string(),
                    Style::default().fg(Color::Rgb(180, 180, 195)),
                ));
                break;
            }
        }
    }

    spans
}

/// Check if a line is a table separator (e.g. |---|---|)
fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|')
        && trimmed.ends_with('|')
        && trimmed.chars().all(|c| c == '|' || c == '-' || c == ':' || c == ' ')
}

/// Check if a line is a table row (e.g. | foo | bar |)
fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.len() > 2
}

/// Render a table row with cells separated by dim pipes.
fn style_table_row(line: &str) -> Line<'static> {
    let trimmed = line.trim();
    // Strip outer pipes and split into cells
    let inner = &trimmed[1..trimmed.len() - 1];
    let cells: Vec<&str> = inner.split('|').map(|c| c.trim()).collect();

    let mut spans = vec![Span::styled(" ", Style::default())];
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                " \u{2502} ",
                Style::default().fg(Color::DarkGray),
            ));
        }
        // Apply inline markdown within cells
        spans.extend(parse_inline_markdown(cell));
    }
    Line::from(spans)
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
