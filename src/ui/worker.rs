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

    // Pinned task banner: one line, dim, never scrolls. Lets the user
    // see "what is this worker doing" even after the original
    // OrcInstruction has scrolled past the visible window.
    let task = session_view.session.task.trim();
    let task_height: u16 = if task.is_empty() || area.height < 6 { 0 } else { 1 };

    let chunks = Layout::vertical([
        Constraint::Length(task_height),
        Constraint::Min(4),
        Constraint::Length(decisions_height),
    ])
    .split(area);

    if task_height > 0 {
        render_task_banner(frame, chunks[0], task);
    }
    render_event_log(
        frame,
        chunks[1],
        &session_view.event_log,
        scroll,
        session_view.is_thinking,
        tick,
        &session_view.session.name,
    );
    if decisions_height > 0 {
        render_decisions(frame, chunks[2], session_view);
    }
}

/// Render a one-line dim banner showing the worker's assigned task.
/// Truncates with ellipsis if it overflows the available width.
fn render_task_banner(frame: &mut Frame, area: Rect, task: &str) {
    if area.width < 8 {
        return;
    }
    let inner_w = area.width as usize;
    let prefix = " task: ";
    let avail = inner_w.saturating_sub(prefix.chars().count() + 1);
    let collapsed: String = task.split_whitespace().collect::<Vec<_>>().join(" ");
    let shown: String = if collapsed.chars().count() > avail {
        let take = avail.saturating_sub(1);
        let mut s: String = collapsed.chars().take(take).collect();
        s.push('…');
        s
    } else {
        collapsed
    };
    let line = Line::from(vec![
        Span::styled(prefix, Style::default().fg(Color::DarkGray)),
        Span::styled(shown, Style::default().fg(Color::Gray).add_modifier(Modifier::ITALIC)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
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

/// Strip ANSI escape sequences and C0/C1 control chars from `s`, preserving
/// `\n` and `\t`. Tool output (e.g. colored bash, git diffs) carries raw
/// escapes; if those bytes land in ratatui cells the terminal interprets
/// them on flush, leaving stray glyphs and cursor jumps that bleed across
/// frames and tabs.
fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    while let Some(pc) = chars.next() {
                        if pc.is_ascii_alphabetic() || pc == '~' {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(pc) = chars.next() {
                        // OSC terminator: BEL or ESC \
                        if pc == '\x07' {
                            break;
                        }
                        if pc == '\x1b' {
                            if matches!(chars.peek(), Some('\\')) {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
        } else if c == '\n' || c == '\t' {
            out.push(c);
        } else {
            let cp = c as u32;
            // Drop C0 (except handled above) and C1 control ranges.
            if cp < 0x20 || (0x7F..=0x9F).contains(&cp) {
                continue;
            }
            out.push(c);
        }
    }
    out
}

/// Tool-call counts accumulated while walking a contiguous run of
/// collapsible tool entries.
#[derive(Default, Clone, Copy)]
struct ToolCounts {
    reads: u32,
    greps: u32,
    lists: u32,
    globs: u32,
    edits: u32,
    writes: u32,
    other: u32,
}

impl ToolCounts {
    fn is_empty(&self) -> bool {
        self.reads + self.greps + self.lists + self.globs + self.edits + self.writes + self.other
            == 0
    }
    fn add(&mut self, bare: &str) {
        match bare {
            "Read" => self.reads += 1,
            "Grep" => self.greps += 1,
            "LS" | "List" => self.lists += 1,
            "Glob" => self.globs += 1,
            "Edit" | "MultiEdit" => self.edits += 1,
            "Write" => self.writes += 1,
            _ => self.other += 1,
        }
    }
    fn sub(&mut self, bare: &str) {
        match bare {
            "Read" => self.reads = self.reads.saturating_sub(1),
            "Grep" => self.greps = self.greps.saturating_sub(1),
            "LS" | "List" => self.lists = self.lists.saturating_sub(1),
            "Glob" => self.globs = self.globs.saturating_sub(1),
            "Edit" | "MultiEdit" => self.edits = self.edits.saturating_sub(1),
            "Write" => self.writes = self.writes.saturating_sub(1),
            _ => self.other = self.other.saturating_sub(1),
        }
    }
    fn parts(&self) -> Vec<String> {
        let mut parts = Vec::new();
        let mut push = |n: u32, sing: &str, plur: &str| {
            if n > 0 {
                parts.push(format!("{} {}", n, if n == 1 { sing } else { plur }));
            }
        };
        push(self.reads, "read", "reads");
        push(self.greps, "grep", "greps");
        push(self.lists, "list", "lists");
        push(self.globs, "glob", "globs");
        push(self.edits, "edit", "edits");
        push(self.writes, "write", "writes");
        push(self.other, "action", "actions");
        parts
    }
}

fn bare_tool_name(name: &str) -> &str {
    name.strip_prefix("mcp__orc__").unwrap_or(name)
}

/// Render unit produced by collapsing the raw event log. Collapsible
/// tool calls fold into `Batch`; Bash, errors, and everything else flow
/// through as `Direct`. `ErrorPair` lifts an errored tool's preceding
/// `ToolUse` out of its batch so the error line has context.
enum Unit<'a> {
    Direct(&'a LogEntry),
    Batch(ToolCounts),
    ErrorPair {
        use_entry: Option<&'a LogEntry>,
        result_entry: &'a LogEntry,
    },
}

/// Collapse successive non-Bash, non-error tool entries into a single
/// summary `Batch`. Bash calls + their results, errored results, and any
/// non-tool entries render as today.
fn collapse_tool_groups(log: &[LogEntry]) -> Vec<Unit<'_>> {
    let mut out: Vec<Unit> = Vec::with_capacity(log.len());
    let mut counts = ToolCounts::default();
    // The most recent collapsed ToolUse (so we can lift it on error) and
    // its bare name (so we can decrement counts on lift).
    let mut last_collapsed: Option<(&LogEntry, String)> = None;
    let mut last_was_bash_use = false;

    let flush = |counts: &mut ToolCounts, out: &mut Vec<Unit>| {
        if !counts.is_empty() {
            out.push(Unit::Batch(*counts));
            *counts = ToolCounts::default();
        }
    };

    for entry in log {
        match entry {
            LogEntry::ToolUse { name, .. } => {
                let bare = bare_tool_name(name);
                if bare == "Bash" {
                    flush(&mut counts, &mut out);
                    last_collapsed = None;
                    last_was_bash_use = true;
                    out.push(Unit::Direct(entry));
                } else {
                    counts.add(bare);
                    last_collapsed = Some((entry, bare.to_string()));
                    last_was_bash_use = false;
                }
            }
            LogEntry::ToolResult { is_error: true, .. } => {
                let lifted = last_collapsed.take();
                if let Some((_, ref bare)) = lifted {
                    counts.sub(bare);
                }
                flush(&mut counts, &mut out);
                out.push(Unit::ErrorPair {
                    use_entry: lifted.map(|(e, _)| e),
                    result_entry: entry,
                });
                last_was_bash_use = false;
            }
            LogEntry::ToolResult { is_error: false, .. } => {
                if last_was_bash_use {
                    out.push(Unit::Direct(entry));
                    last_was_bash_use = false;
                }
                // Otherwise: success result for a collapsed tool — counted
                // in its ToolUse, drop the body.
                last_collapsed = None;
            }
            LogEntry::Exploration {
                reads,
                greps,
                lists,
                globs,
            } => {
                counts.reads += reads;
                counts.greps += greps;
                counts.lists += lists;
                counts.globs += globs;
                last_collapsed = None;
                last_was_bash_use = false;
            }
            _ => {
                flush(&mut counts, &mut out);
                last_collapsed = None;
                last_was_bash_use = false;
                out.push(Unit::Direct(entry));
            }
        }
    }
    flush(&mut counts, &mut out);
    out
}

/// Convert a structured log into renderable lines. Used by both the orc tab
/// and worker tabs — same model for both. `speaker` is the name shown as
/// the prefix on `AssistantText` lines (worker name on a worker tab,
/// `"orc"` on the orc tab).
pub fn log_lines(log: &[LogEntry], speaker: &str) -> Vec<Line<'static>> {
    let units = collapse_tool_groups(log);
    let mut out: Vec<Line<'static>> = Vec::with_capacity(units.len());

    let push_gap = |out: &mut Vec<Line<'static>>| {
        if !out.is_empty()
            && !out
                .last()
                .map(|l| l.spans.iter().all(|s| s.content.is_empty()))
                .unwrap_or(true)
        {
            out.push(Line::from(""));
        }
    };

    // Group identifier for gap-on-transition logic. Higher = "more
    // disruptive" but only equality matters.
    fn unit_group(u: &Unit) -> u8 {
        match u {
            Unit::Direct(e) => match e {
                LogEntry::UserText(_) | LogEntry::OrcInstruction(_) | LogEntry::AssistantText(_) => 1,
                LogEntry::ToolUse { .. } | LogEntry::ToolResult { .. } | LogEntry::Exploration { .. } => 2,
                LogEntry::Thinking(_) => 3,
                LogEntry::System(_) => 4,
                LogEntry::Notify(_) => 5,
                LogEntry::TurnEnd { .. } => 0,
            },
            Unit::Batch(_) | Unit::ErrorPair { .. } => 2,
        }
    }

    let mut prev_group: Option<u8> = None;
    for unit in &units {
        let g = unit_group(unit);
        if g != 0 {
            match prev_group {
                Some(prev) if prev != g => push_gap(&mut out),
                Some(_) if g == 1 => push_gap(&mut out),
                _ => {}
            }
        }
        prev_group = Some(g);
        match unit {
            Unit::Batch(c) => render_batch_line(&mut out, c),
            Unit::ErrorPair {
                use_entry,
                result_entry,
            } => {
                if let Some(use_e) = use_entry {
                    render_entry(&mut out, use_e, speaker);
                }
                render_entry(&mut out, result_entry, speaker);
            }
            Unit::Direct(e) => render_entry(&mut out, e, speaker),
        }
    }
    out
}

/// Emit one dim summary line for a collapsed tool batch. No leading sigil
/// — keeps it visually distinct from system (`·`) and notify (`●`).
fn render_batch_line(out: &mut Vec<Line<'static>>, counts: &ToolCounts) {
    let parts = counts.parts();
    if parts.is_empty() {
        return;
    }
    out.push(Line::from(Span::styled(
        format!("(did: {})", parts.join(", ")),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )));
}

/// Render a single LogEntry to one-or-more output lines. Extracted so
/// `ErrorPair` can render its two member entries with the same path.
fn render_entry(out: &mut Vec<Line<'static>>, entry: &LogEntry, speaker: &str) {
    match entry {
        LogEntry::UserText(t) => {
            let t = sanitize(t);
            push_prefixed_lines(
                out,
                "you   ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                &t,
                Style::default(),
            );
        }
        LogEntry::OrcInstruction(t) => {
            let t = sanitize(t);
            push_prefixed_lines(
                out,
                "orc → ",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
                &t,
                Style::default(),
            );
        }
        LogEntry::AssistantText(t) => {
            let t = sanitize(t);
            let mut md = render_markdown(&t);
            let prefix = format!("{speaker} ");
            prepend_speaker(
                &mut md,
                &prefix,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            );
            for line in md {
                out.push(line);
            }
        }
        LogEntry::Thinking(t) => {
            let t = sanitize(t);
            let trimmed = t.trim();
            if trimmed.is_empty() {
                return;
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
            Span::raw(format!("{}{}", sanitize(name), sanitize(input_summary))),
        ])),
        LogEntry::ToolResult { text, is_error } => {
            let color = if *is_error { Color::Red } else { Color::Green };
            let text = sanitize(text);
            let lines: Vec<&str> = text.split('\n').collect();
            let cap = 5usize;
            let body_style = if *is_error {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            let arrow_style = Style::default().fg(color);
            let n = lines.len();
            for (i, ln) in lines.iter().enumerate().take(cap) {
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
            let s = sanitize(s);
            for (i, ln) in s.split('\n').enumerate() {
                let prefix = if i == 0 { "· " } else { "  " };
                out.push(Line::from(Span::styled(
                    format!("{prefix}{ln}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
        LogEntry::Notify(s) => {
            let s = sanitize(s);
            for (i, ln) in s.split('\n').enumerate() {
                let mut spans: Vec<Span<'static>> = Vec::new();
                if i == 0 {
                    spans.push(Span::styled(
                        "● ",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::raw("  "));
                }
                spans.push(Span::styled(
                    ln.to_string(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ));
                out.push(Line::from(spans));
            }
        }
        LogEntry::TurnEnd { cost_usd: _ } => {
            out.push(Line::from(""));
        }
        LogEntry::Exploration {
            reads,
            greps,
            lists,
            globs,
        } => {
            // Should not reach here under collapse (Exploration merges
            // into a Batch), but render defensively if it does.
            let counts = ToolCounts {
                reads: *reads,
                greps: *greps,
                lists: *lists,
                globs: *globs,
                ..Default::default()
            };
            render_batch_line(out, &counts);
        }
    }
}

/// Prepend a speaker prefix to the first non-empty line of a markdown
/// block and indent every other non-empty line by the prefix's width.
/// Empty paragraph-separator lines are left untouched so blank gaps
/// stay blank.
fn prepend_speaker(lines: &mut [Line<'static>], prefix: &str, prefix_style: Style) {
    let indent = " ".repeat(prefix.chars().count());
    let mut prefixed = false;
    for line in lines.iter_mut() {
        let has_content = line.spans.iter().any(|s| !s.content.is_empty());
        if !has_content {
            continue;
        }
        let mut spans = std::mem::take(&mut line.spans);
        if !prefixed {
            spans.insert(0, Span::styled(prefix.to_string(), prefix_style));
            prefixed = true;
        } else {
            spans.insert(0, Span::raw(indent.clone()));
        }
        line.spans = spans;
    }
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

/// Render assistant text via `pulldown-cmark` into ratatui Lines. Handles
/// headings, emphasis, code (inline + fenced), lists, blockquotes, links,
/// and horizontal rules. Soft breaks render as spaces; hard breaks and
/// block-level boundaries flush the current line.
fn render_markdown(text: &str) -> Vec<Line<'static>> {
    use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

    let parser = Parser::new(text);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut style_stack: Vec<Style> = vec![Style::default()];
    // Track structural state.
    let mut in_code_block = false;
    let mut list_depth: usize = 0;
    let mut list_index_stack: Vec<Option<u64>> = Vec::new();
    let mut item_pending_marker = false;
    let mut block_just_ended = false;

    let style = |stack: &Vec<Style>| -> Style {
        stack.last().copied().unwrap_or_default()
    };
    // Pop without underflow — base Style at stack[0] is preserved.
    let pop = |stack: &mut Vec<Style>| {
        if stack.len() > 1 {
            stack.pop();
        }
    };

    let flush =
        |current: &mut Vec<Span<'static>>, lines: &mut Vec<Line<'static>>| {
            if current.is_empty() {
                lines.push(Line::from(""));
            } else {
                lines.push(Line::from(std::mem::take(current)));
            }
        };

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    if !current.is_empty() {
                        flush(&mut current, &mut lines);
                    }
                    if !lines.is_empty() && !block_just_ended {
                        lines.push(Line::from(""));
                    }
                    let mut s = Style::default().add_modifier(Modifier::BOLD);
                    if matches!(level, HeadingLevel::H1 | HeadingLevel::H2) {
                        s = s.fg(Color::Cyan);
                    }
                    style_stack.push(s);
                    block_just_ended = false;
                }
                Tag::Paragraph => {
                    if !current.is_empty() {
                        flush(&mut current, &mut lines);
                    }
                    if !lines.is_empty() && !block_just_ended {
                        lines.push(Line::from(""));
                    }
                    block_just_ended = false;
                }
                Tag::BlockQuote(_) => {
                    if !current.is_empty() {
                        flush(&mut current, &mut lines);
                    }
                    let s = style(&style_stack)
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC);
                    style_stack.push(s);
                }
                Tag::CodeBlock(_) => {
                    if !current.is_empty() {
                        flush(&mut current, &mut lines);
                    }
                    in_code_block = true;
                    style_stack.push(Style::default().fg(Color::LightYellow));
                }
                Tag::List(start) => {
                    if !current.is_empty() {
                        flush(&mut current, &mut lines);
                    }
                    list_depth += 1;
                    list_index_stack.push(start);
                }
                Tag::Item => {
                    if !current.is_empty() {
                        flush(&mut current, &mut lines);
                    }
                    item_pending_marker = true;
                }
                Tag::Emphasis => {
                    let s = style(&style_stack).add_modifier(Modifier::ITALIC);
                    style_stack.push(s);
                }
                Tag::Strong => {
                    let s = style(&style_stack).add_modifier(Modifier::BOLD);
                    style_stack.push(s);
                }
                Tag::Strikethrough => {
                    let s = style(&style_stack).add_modifier(Modifier::CROSSED_OUT);
                    style_stack.push(s);
                }
                Tag::Link { .. } => {
                    let s = style(&style_stack)
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::UNDERLINED);
                    style_stack.push(s);
                }
                _ => {
                    style_stack.push(style(&style_stack));
                }
            },
            Event::End(end) => match end {
                TagEnd::Heading(_) | TagEnd::Paragraph => {
                    pop(&mut style_stack);
                    flush(&mut current, &mut lines);
                    block_just_ended = true;
                }
                TagEnd::BlockQuote(_) => {
                    pop(&mut style_stack);
                    if !current.is_empty() {
                        flush(&mut current, &mut lines);
                    }
                    block_just_ended = true;
                }
                TagEnd::CodeBlock => {
                    pop(&mut style_stack);
                    in_code_block = false;
                    if !current.is_empty() {
                        flush(&mut current, &mut lines);
                    }
                    block_just_ended = true;
                }
                TagEnd::List(_) => {
                    list_depth = list_depth.saturating_sub(1);
                    list_index_stack.pop();
                    if list_depth == 0 {
                        block_just_ended = true;
                    }
                }
                TagEnd::Item => {
                    if !current.is_empty() {
                        flush(&mut current, &mut lines);
                    }
                }
                TagEnd::Emphasis
                | TagEnd::Strong
                | TagEnd::Strikethrough
                | TagEnd::Link => {
                    pop(&mut style_stack);
                }
                _ => {
                    pop(&mut style_stack);
                }
            },
            Event::Text(t) => {
                if item_pending_marker {
                    let indent = "  ".repeat(list_depth.saturating_sub(1));
                    let last_idx = list_index_stack.len().saturating_sub(1);
                    let marker = match list_index_stack.get_mut(last_idx) {
                        Some(Some(n)) => {
                            let m = format!("{n}. ");
                            *n += 1;
                            m
                        }
                        _ => "• ".to_string(),
                    };
                    current.push(Span::raw(format!("{indent}{marker}")));
                    item_pending_marker = false;
                }
                if in_code_block {
                    let body = t.into_string();
                    for (i, ln) in body.split('\n').enumerate() {
                        if i > 0 {
                            flush(&mut current, &mut lines);
                        }
                        if !ln.is_empty() {
                            current.push(Span::styled(
                                ln.to_string(),
                                style(&style_stack),
                            ));
                        }
                    }
                } else {
                    current.push(Span::styled(t.into_string(), style(&style_stack)));
                }
                block_just_ended = false;
            }
            Event::Code(c) => {
                // Inline code: use a calm Cyan over the previous bright
                // LightYellow. With many code tokens per paragraph the
                // yellow created a noisy "rainbow" effect; cyan reads as
                // monospace technical terms without fighting body text.
                current.push(Span::styled(
                    c.into_string(),
                    style(&style_stack).fg(Color::Cyan),
                ));
            }
            Event::SoftBreak => {
                current.push(Span::raw(" "));
            }
            Event::HardBreak => {
                flush(&mut current, &mut lines);
            }
            Event::Rule => {
                if !current.is_empty() {
                    flush(&mut current, &mut lines);
                }
                lines.push(Line::from(Span::styled(
                    "─".repeat(40),
                    Style::default().fg(Color::DarkGray),
                )));
                block_just_ended = true;
            }
            _ => {}
        }
    }

    if !current.is_empty() {
        flush(&mut current, &mut lines);
    }
    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

pub fn render_event_log(
    frame: &mut Frame,
    area: Rect,
    log: &[LogEntry],
    scroll: usize,
    thinking: bool,
    tick: u64,
    speaker: &str,
) {
    let mut lines = log_lines(log, speaker);
    if thinking {
        lines.push(thinking_line(tick));
    }
    let total_lines = lines.len();

    let block_inner_w = area.width.saturating_sub(2) as usize;
    // Pre-wrap into terminal rows ourselves. Paragraph's built-in
    // Wrap+scroll has glitches under autoscroll while a worker is
    // streaming — half-overwritten cells leak across frames. With
    // pre-wrapped rows we render a simple slice, no internal wrap/scroll.
    let rows = wrap_lines_to_rows(&lines, block_inner_w);
    let wrapped = rows.len();

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

    // Belt-and-braces clear of inner before rendering the slice.
    frame.render_widget(Clear, inner);

    let visible: Vec<Line<'static>> = rows
        .into_iter()
        .skip(scroll_clamped)
        .take(inner_h)
        .collect();
    let para = Paragraph::new(visible);
    frame.render_widget(para, inner);
}

/// Pre-wrap a list of logical Lines into terminal rows. Each output Line
/// is exactly one row of width <= `width`. Span boundaries are preserved
/// where possible; long spans are split mid-string at column boundaries.
/// Empty lines round-trip as empty rows.
pub fn wrap_lines_to_rows(lines: &[Line<'static>], width: usize) -> Vec<Line<'static>> {
    use unicode_width::UnicodeWidthChar;

    if width == 0 {
        return lines.iter().cloned().collect();
    }

    let mut out: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    for src in lines {
        let line_style = src.style;
        let mut cur: Vec<Span<'static>> = Vec::new();
        let mut cur_w: usize = 0;
        let mut emitted_for_line = false;

        for span in &src.spans {
            let style = span.style;
            let mut buf = String::new();
            for ch in span.content.chars() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if cw > 0 && cur_w + cw > width {
                    if !buf.is_empty() {
                        cur.push(Span::styled(std::mem::take(&mut buf), style));
                    }
                    out.push(Line::from(std::mem::take(&mut cur)).style(line_style));
                    cur_w = 0;
                    emitted_for_line = true;
                }
                buf.push(ch);
                cur_w += cw;
            }
            if !buf.is_empty() {
                cur.push(Span::styled(buf, style));
            }
        }

        if !cur.is_empty() || !emitted_for_line {
            out.push(Line::from(cur).style(line_style));
        }
    }
    out
}

/// Build the events frame title. When all rows fit on screen, just
/// ` events `. When scrolled, show a compact position indicator
/// ` events  ▲ N% ` so the eye doesn't have to parse "N–M / X / Y".
fn format_events_title(
    scroll: usize,
    visible: usize,
    wrapped_total: usize,
    _entries_total: usize,
) -> String {
    if wrapped_total == 0 || wrapped_total <= visible {
        return " events ".to_string();
    }
    let max_scroll = wrapped_total.saturating_sub(visible);
    if scroll >= max_scroll {
        return " events  · end ".to_string();
    }
    let pct = if max_scroll == 0 {
        0
    } else {
        (scroll * 100) / max_scroll
    };
    format!(" events  · {pct}% ↑{}  ", max_scroll - scroll)
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
pub fn wrapped_line_count(log: &[LogEntry], inner_width: u16, speaker: &str) -> usize {
    let lines = log_lines(log, speaker);
    wrap_lines_to_rows(&lines, inner_width as usize).len()
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
            last_context_tokens: None,
            compose: crate::app::ComposeState::default(),
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
