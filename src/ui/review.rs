// Review tab: diff viewer for agent output.
//
// Two-pane layout: file tree (25%) | diff view (75%).
// Bottom action bar rendered as part of the widget.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::review::*;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn render_review(frame: &mut Frame, area: Rect, review: &ReviewState) {
    if area.height < 3 || area.width < 10 {
        return;
    }

    // Vertical split: main content | action bar
    let vsplit = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);

    // Horizontal split: file tree (25%) | diff view (75%)
    let hsplit = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
        .split(vsplit[0]);

    render_file_tree(frame, hsplit[0], review);
    match review.view_mode {
        ViewMode::Diff => render_diff_view(frame, hsplit[1], review),
        ViewMode::WholeFile => render_whole_file_view(frame, hsplit[1], review),
    }
    render_review_action_bar(frame, vsplit[1]);
}

// ---------------------------------------------------------------------------
// File tree pane
// ---------------------------------------------------------------------------

fn render_file_tree(frame: &mut Frame, area: Rect, review: &ReviewState) {
    let block = Block::default()
        .title(" files ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 2 || inner.height < 1 {
        return;
    }

    let files = &review.diff.files;
    let mut lines: Vec<Line> = Vec::new();

    // Collect which files have comments
    let files_with_comments: std::collections::HashSet<&str> = review
        .comments
        .iter()
        .map(|c| c.file.as_str())
        .collect();

    for (idx, file) in files.iter().enumerate() {
        let has_comment = files_with_comments.contains(file.path.as_str());
        let marker = if has_comment { "\u{25cf}" } else { "\u{25cc}" }; // ● or ◌
        let is_focused = idx == review.cursor.file_idx;

        let style = if is_focused {
            Style::default()
                .fg(Color::Black)
                .bg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let stats = format!(
            " +{} -{}",
            file.additions, file.deletions
        );

        let path_display = truncate_path(&file.path, inner.width.saturating_sub(stats.len() as u16 + 3) as usize);

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {marker} "),
                if has_comment {
                    style.fg(Color::Yellow)
                } else {
                    style.fg(Color::DarkGray)
                },
            ),
            Span::styled(path_display, style),
            Span::styled(
                stats,
                style.fg(Color::DarkGray),
            ),
        ]));
    }

    // Summary at bottom
    let total_add: usize = files.iter().map(|f| f.additions).sum();
    let total_del: usize = files.iter().map(|f| f.deletions).sum();
    let comment_count = review.comments.len();
    let approval_count = review.hunk_approvals.len();

    // Pad to push summary to bottom
    let content_lines = lines.len();
    let available = inner.height as usize;
    if content_lines + 3 < available {
        for _ in 0..(available - content_lines - 3) {
            lines.push(Line::from(""));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} files +{} -{}", files.len(), total_add, total_del),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} comments  {} approvals", comment_count, approval_count),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

/// Truncate a file path to fit, preferring the filename end.
fn truncate_path(path: &str, max: usize) -> String {
    if path.len() <= max {
        path.to_string()
    } else if max > 3 {
        format!("...{}", &path[path.len().saturating_sub(max - 3)..])
    } else {
        path[..max].to_string()
    }
}

// ---------------------------------------------------------------------------
// Whole-file view pane
// ---------------------------------------------------------------------------

fn render_whole_file_view(frame: &mut Frame, area: Rect, review: &ReviewState) {
    let file = match review.current_file() {
        Some(f) => f,
        None => {
            let block = Block::default()
                .title(" file ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray));
            frame.render_widget(block, area);
            return;
        }
    };

    let title = format!(" file: {} ", file.path);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 4 || inner.height < 1 {
        return;
    }

    // Resolve absolute path: worktree_path / file.path
    let abs_path = if review.worktree_path.is_empty() {
        std::path::PathBuf::from(&file.path)
    } else {
        std::path::PathBuf::from(&review.worktree_path).join(&file.path)
    };

    let body = match std::fs::read_to_string(&abs_path) {
        Ok(s) => s,
        Err(e) => {
            let p = Paragraph::new(format!("  cannot read {}: {e}", abs_path.display()))
                .style(Style::default().fg(Color::Red));
            frame.render_widget(p, inner);
            return;
        }
    };

    // Lines that have a draft comment in this file.
    let commented: std::collections::HashSet<usize> = review
        .comments
        .iter()
        .filter(|c| c.file == file.path)
        .map(|c| c.line)
        .collect();

    // Compute cursor's source-file line, if any.
    let cursor_lineno: Option<usize> = review
        .current_line()
        .and_then(|l| l.new_lineno.or(l.old_lineno));

    let mut lines: Vec<Line> = Vec::new();
    for (i, content) in body.lines().enumerate() {
        let lineno = i + 1;
        let gutter = if commented.contains(&lineno) { "*" } else { " " };
        let is_cursor = cursor_lineno == Some(lineno);
        let row_style = if is_cursor {
            Style::default().fg(Color::Black).bg(Color::White)
        } else {
            Style::default()
        };
        let truncated = truncate_content(
            content,
            inner.width.saturating_sub(8) as usize,
        );
        lines.push(Line::from(vec![
            Span::styled(format!(" {gutter} "), Style::default().fg(Color::Yellow)),
            Span::styled(format!("{:>4} ", lineno), Style::default().fg(Color::DarkGray)),
            Span::styled(truncated, row_style),
        ]));
    }

    // Scroll: keep cursor visible
    let cursor_idx = cursor_lineno.map(|n| n.saturating_sub(1)).unwrap_or(0);
    let visible = inner.height as usize;
    let scroll_offset = if cursor_idx >= visible {
        cursor_idx.saturating_sub(visible / 2)
    } else {
        0
    };

    // Force-clear inner cells first — Paragraph with Wrap+scroll can leave
    // stray cells from prior frames (matches the workaround in
    // worker::render_event_log).
    frame.render_widget(Clear, inner);

    let paragraph = Paragraph::new(lines)
        .scroll((scroll_offset as u16, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

// ---------------------------------------------------------------------------
// Diff view pane
// ---------------------------------------------------------------------------

fn render_diff_view(frame: &mut Frame, area: Rect, review: &ReviewState) {
    let files = &review.diff.files;
    let file_title = if let Some(f) = files.get(review.cursor.file_idx) {
        format!(" diff: {} ", f.path)
    } else {
        " diff ".to_string()
    };

    let block = Block::default()
        .title(file_title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 4 || inner.height < 1 {
        return;
    }

    let file = match files.get(review.cursor.file_idx) {
        Some(f) => f,
        None => {
            let empty = Paragraph::new("  no files in diff")
                .style(Style::default().fg(Color::DarkGray));
            frame.render_widget(empty, inner);
            return;
        }
    };

    // Build lines for all hunks in this file
    let mut lines: Vec<Line> = Vec::new();
    let mut flat_line_idx: usize = 0;

    // Index comments by (file, line) for quick lookup
    let comment_lines: std::collections::HashMap<usize, Vec<&DraftComment>> = {
        let mut map: std::collections::HashMap<usize, Vec<&DraftComment>> = std::collections::HashMap::new();
        for c in &review.comments {
            if c.file == file.path {
                map.entry(c.line).or_default().push(c);
            }
        }
        map
    };

    for hunk in &file.hunks {
        let is_approved = review.hunk_approvals.contains(&hunk.id);

        // Hunk header line
        let header_marker = if is_approved { " \u{2713} " } else { "   " };
        lines.push(Line::from(vec![
            Span::styled(
                header_marker,
                Style::default().fg(Color::Green),
            ),
            Span::styled(
                &hunk.header,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        flat_line_idx += 1;

        // Diff lines
        for diff_line in &hunk.lines {
            let is_cursor = flat_line_idx == review.cursor.line_idx;

            let old_no: String = diff_line
                .old_lineno
                .map(|n| format!("{:>4}", n))
                .unwrap_or_else(|| "    ".to_string());
            let new_no: String = diff_line
                .new_lineno
                .map(|n| format!("{:>4}", n))
                .unwrap_or_else(|| "    ".to_string());

            let (prefix, line_color) = match diff_line.kind {
                LineKind::Add => (" + ", Color::Green),
                LineKind::Remove => (" - ", Color::Red),
                LineKind::Context => ("   ", Color::Reset),
            };

            let cursor_marker = if is_cursor { "\u{25b6}" } else { " " };

            // Check for comment on this line
            let has_comment = diff_line.new_lineno.map_or(false, |n| comment_lines.contains_key(&n));
            let comment_marker = if has_comment { " \u{1f4ac}" } else { "" };

            let base_style = if is_cursor {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::White)
            } else {
                Style::default().fg(line_color)
            };

            lines.push(Line::from(vec![
                Span::styled(cursor_marker.to_string(), base_style.add_modifier(Modifier::BOLD)),
                Span::styled(old_no, Style::default().fg(Color::DarkGray)),
                Span::styled(" ", Style::default()),
                Span::styled(new_no, Style::default().fg(Color::DarkGray)),
                Span::styled(prefix, base_style),
                Span::styled(
                    truncate_content(&diff_line.content, inner.width.saturating_sub(16) as usize),
                    base_style,
                ),
                Span::raw(comment_marker),
            ]));

            // Render inline comments below commented line
            if let Some(lineno) = diff_line.new_lineno {
                if let Some(comments) = comment_lines.get(&lineno) {
                    for comment in comments {
                        lines.push(Line::from(vec![
                            Span::raw("             "),
                            Span::styled(
                                "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]));
                        // Wrap comment body
                        let body_width = inner.width.saturating_sub(18) as usize;
                        for body_line in wrap_text(&comment.body, body_width) {
                            lines.push(Line::from(vec![
                                Span::raw("             "),
                                Span::styled(
                                    format!("\u{1f4ac} {}", body_line),
                                    Style::default().fg(Color::Yellow),
                                ),
                            ]));
                        }
                    }
                }
            }

            flat_line_idx += 1;
        }
    }

    // Scroll: keep cursor visible
    let visible = inner.height as usize;
    let scroll_offset = if review.cursor.line_idx >= visible {
        review.cursor.line_idx.saturating_sub(visible / 2)
    } else {
        0
    };

    // Force-clear inner cells first — see render_whole_file_view comment.
    frame.render_widget(Clear, inner);

    let paragraph = Paragraph::new(lines)
        .scroll((scroll_offset as u16, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

fn truncate_content(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max > 3 {
        format!("{}...", &s[..max - 3])
    } else {
        s[..max].to_string()
    }
}

/// Simple word-wrapping for comment bodies.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut result = Vec::new();
    for line in text.lines() {
        if line.len() <= width {
            result.push(line.to_string());
        } else {
            let mut pos = 0;
            while pos < line.len() {
                let end = (pos + width).min(line.len());
                result.push(line[pos..end].to_string());
                pos = end;
            }
        }
    }
    if result.is_empty() {
        result.push(String::new());
    }
    result
}

// ---------------------------------------------------------------------------
// Action bar
// ---------------------------------------------------------------------------

fn render_review_action_bar(frame: &mut Frame, area: Rect) {
    let bar = Paragraph::new(Line::from(vec![
        Span::styled(" j", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled("/", Style::default().fg(Color::DarkGray)),
        Span::styled("k", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(" line  ", Style::default().fg(Color::DarkGray)),
        Span::styled("J", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled("/", Style::default().fg(Color::DarkGray)),
        Span::styled("K", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(" hunk  ", Style::default().fg(Color::DarkGray)),
        Span::styled("[", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled("/", Style::default().fg(Color::DarkGray)),
        Span::styled("]", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(" file  ", Style::default().fg(Color::DarkGray)),
        Span::styled("c", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(" comment  ", Style::default().fg(Color::DarkGray)),
        Span::styled("a", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(" approve  ", Style::default().fg(Color::DarkGray)),
        Span::styled("o", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(" file-view  ", Style::default().fg(Color::DarkGray)),
        Span::styled("e", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(" editor  ", Style::default().fg(Color::DarkGray)),
        Span::styled("s", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(" submit  ", Style::default().fg(Color::DarkGray)),
        Span::styled("q", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
    ]))
    .style(Style::default().bg(Color::Black));

    frame.render_widget(bar, area);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::HashSet;

    fn empty_review() -> ReviewState {
        ReviewState {
            session_id: "s1".to_string(),
            diff: ParsedDiff::empty(),
            diff_hash: "abc123".to_string(),
            cursor: DiffCursor::new(),
            comments: Vec::new(),
            hunk_approvals: HashSet::new(),
            overall: None,
            view_mode: crate::review::ViewMode::Diff,
            worktree_path: String::new(),
        }
    }

    fn multi_file_review() -> ReviewState {
        ReviewState {
            session_id: "s1".to_string(),
            diff: ParsedDiff {
                files: vec![
                    DiffFile {
                        path: "src/services/uom.rs".to_string(),
                        hunks: vec![Hunk {
                            id: "h1".to_string(),
                            header: "@@ -10,6 +10,8 @@ pub struct Uom".to_string(),
                            lines: vec![
                                DiffLine {
                                    kind: LineKind::Context,
                                    content: "pub id: Uuid,".to_string(),
                                    old_lineno: Some(12),
                                    new_lineno: Some(12),
                                },
                                DiffLine {
                                    kind: LineKind::Context,
                                    content: "pub name: String,".to_string(),
                                    old_lineno: Some(13),
                                    new_lineno: Some(13),
                                },
                                DiffLine {
                                    kind: LineKind::Add,
                                    content: "pub conversion_factor: f64,".to_string(),
                                    old_lineno: None,
                                    new_lineno: Some(15),
                                },
                                DiffLine {
                                    kind: LineKind::Add,
                                    content: "pub base_unit_id: Option<Uuid>,".to_string(),
                                    old_lineno: None,
                                    new_lineno: Some(16),
                                },
                                DiffLine {
                                    kind: LineKind::Remove,
                                    content: "pub old_field: bool,".to_string(),
                                    old_lineno: Some(14),
                                    new_lineno: None,
                                },
                            ],
                            old_start: 10,
                            new_start: 10,
                        }],
                        additions: 47,
                        deletions: 3,
                    },
                    DiffFile {
                        path: "tests/uom_test.rs".to_string(),
                        hunks: vec![Hunk {
                            id: "h2".to_string(),
                            header: "@@ -1,0 +1,89 @@".to_string(),
                            lines: vec![
                                DiffLine {
                                    kind: LineKind::Add,
                                    content: "mod tests {".to_string(),
                                    old_lineno: None,
                                    new_lineno: Some(1),
                                },
                                DiffLine {
                                    kind: LineKind::Add,
                                    content: "    #[test]".to_string(),
                                    old_lineno: None,
                                    new_lineno: Some(2),
                                },
                            ],
                            old_start: 1,
                            new_start: 1,
                        }],
                        additions: 89,
                        deletions: 0,
                    },
                ],
            },
            diff_hash: "deadbeef".to_string(),
            cursor: DiffCursor::new(),
            comments: Vec::new(),
            hunk_approvals: HashSet::new(),
            overall: None,
            view_mode: crate::review::ViewMode::Diff,
            worktree_path: String::new(),
        }
    }

    fn review_with_comments_and_approvals() -> ReviewState {
        let mut review = multi_file_review();
        review.comments.push(DraftComment {
            file: "src/services/uom.rs".to_string(),
            line: 16,
            body: "should we make base_unit_id required when conversion_factor != 1.0?".to_string(),
        });
        review.hunk_approvals.insert("h2".to_string());
        review
    }

    #[test]
    fn render_empty_diff_no_panic() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let review = empty_review();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_review(frame, area, &review);
            })
            .unwrap();
    }

    #[test]
    fn render_multi_file_no_panic() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let review = multi_file_review();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_review(frame, area, &review);
            })
            .unwrap();
    }

    #[test]
    fn render_with_comments_and_approvals_no_panic() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let review = review_with_comments_and_approvals();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_review(frame, area, &review);
            })
            .unwrap();
    }

    #[test]
    fn render_small_terminal_no_panic() {
        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let review = multi_file_review();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_review(frame, area, &review);
            })
            .unwrap();
    }

    #[test]
    fn render_tiny_terminal_no_panic() {
        let backend = TestBackend::new(8, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        let review = multi_file_review();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_review(frame, area, &review);
            })
            .unwrap();
    }

    #[test]
    fn file_tree_shows_file_paths() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let review = multi_file_review();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_review(frame, area, &review);
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let content: String = (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
            .collect();

        assert!(content.contains("uom.rs"), "file tree should show uom.rs");
        assert!(content.contains("uom_test.rs"), "file tree should show uom_test.rs");
    }

    #[test]
    fn diff_view_shows_current_file() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let review = multi_file_review();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_review(frame, area, &review);
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let top_line: String = (0..buf.area.width)
            .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
            .collect();

        assert!(
            top_line.contains("diff") || top_line.contains("files"),
            "header should contain pane titles"
        );
    }

    #[test]
    fn action_bar_shows_keybinds() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let review = empty_review();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_review(frame, area, &review);
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let last_line: String = (0..buf.area.width)
            .map(|x| buf.cell((x, buf.area.height - 1)).unwrap().symbol().to_string())
            .collect();

        assert!(last_line.contains("comment"), "action bar should show comment hint");
        assert!(last_line.contains("approve"), "action bar should show approve hint");
        assert!(last_line.contains("submit"), "action bar should show submit hint");
        assert!(last_line.contains("cancel"), "action bar should show cancel hint");
    }

    #[test]
    fn comment_marker_present() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let review = review_with_comments_and_approvals();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_review(frame, area, &review);
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let content: String = (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
            .collect();

        // Comment body should appear inline
        assert!(
            content.contains("base_unit_id"),
            "inline comment body should be visible"
        );
    }

    #[test]
    fn file_tree_summary_stats() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let review = review_with_comments_and_approvals();

        terminal
            .draw(|frame| {
                let area = frame.area();
                render_review(frame, area, &review);
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let content: String = (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
            .collect();

        assert!(content.contains("2 files"), "should show file count");
        assert!(content.contains("1 comments"), "should show comment count");
        assert!(content.contains("1 approvals"), "should show approval count");
    }

    #[test]
    fn wrap_text_basic() {
        assert_eq!(wrap_text("hello", 10), vec!["hello"]);
        assert_eq!(wrap_text("hello world", 5), vec!["hello", " worl", "d"]);
        assert_eq!(wrap_text("", 10), vec![""]);
    }

    #[test]
    fn truncate_path_basic() {
        assert_eq!(truncate_path("src/main.rs", 20), "src/main.rs");
        assert_eq!(truncate_path("very/long/path/to/file.rs", 15), "...h/to/file.rs");
    }
}
