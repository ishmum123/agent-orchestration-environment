// TUI integration tests
//
// L3: ratatui TestBackend rendering — verify layout and content without a terminal.
// L4: Drive orc binary in tmux — verify pane content shows expected UI elements.

use orc::app::{App, ChatRole, Modal, TabId};
use orc::review::{
    DiffCursor, DiffFile, DiffLine, Hunk, LineKind, ParsedDiff, ReviewState,
};
use orc::session::{BlockKind, Session, SessionMode, SessionState};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_session(id: &str, name: &str, state: SessionState) -> Session {
    Session {
        id: id.to_string(),
        name: name.to_string(),
        task: "test task".to_string(),
        worktree_path: "/tmp/wt".to_string(),
        branch: "test-branch".to_string(),
        base_commit: "abc123".to_string(),
        tmux_session: format!("orc-{name}"),
        state,
        mode: SessionMode::Watch,
        model: "sonnet".to_string(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        ended_at: None,
            claude_session_id: None,
    }
}

fn render_to_string(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| orc::ui::render(f, app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let mut output = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            output.push_str(buf.cell((x, y)).unwrap().symbol());
        }
        output.push('\n');
    }
    output
}

// ---------------------------------------------------------------------------
// L3: TestBackend rendering tests
// ---------------------------------------------------------------------------

#[test]
fn l3_empty_dashboard_renders() {
    let mut app = App::new(".");
    let output = render_to_string(&mut app, 80, 24);
    // After UX overhaul: agents panel always shows orc card.
    assert!(output.contains("orc"), "should show orc agent card");
    assert!(
        output.contains("quit") || output.contains("help"),
        "should show action bar hints"
    );
}

#[test]
fn l3_dashboard_with_chat() {
    let mut app = App::new(".");
    app.push_chat(ChatRole::User, "deploy to staging".to_string());
    app.push_chat(ChatRole::Orc, "planning 3 sessions".to_string());
    let output = render_to_string(&mut app, 120, 40);
    assert!(output.contains("deploy to staging"), "should show user message");
    assert!(output.contains("planning 3 sessions"), "should show orc message");
}

#[test]
fn l3_worker_tabs_show_badges() {
    let mut app = App::new(".");
    app.add_session(test_session("s1", "auth-fix", SessionState::Running));
    app.add_session(test_session(
        "s2",
        "test-write",
        SessionState::Done {
            summary: "done".to_string(),
        },
    ));
    app.add_session(test_session(
        "s3",
        "migration",
        SessionState::Blocked {
            kind: BlockKind::Permission,
            reason: "write outside worktree".to_string(),
        },
    ));

    let output = render_to_string(&mut app, 120, 40);
    assert!(output.contains("auth-fix"), "should show worker name");
    assert!(output.contains("test-write"), "should show second worker");
    assert!(output.contains("◐"), "running badge");
    assert!(output.contains("✓"), "done badge");
    assert!(output.contains("?"), "permission badge");
}

#[test]
fn l3_worker_tab_focused() {
    let mut app = App::new(".");
    app.add_session(test_session("s1", "auth-fix", SessionState::Running));
    app.sessions[0]
        .event_log
        .push(orc::app::LogEntry::AssistantText("reading src/auth.rs".to_string()));
    app.sessions[0]
        .event_log
        .push(orc::app::LogEntry::AssistantText("found 3 handlers".to_string()));

    app.focus_tab(TabId::Worker(0));
    let output = render_to_string(&mut app, 120, 40);
    assert!(output.contains("auth-fix"), "should show session name");
    assert!(output.contains("kill"), "action bar should show kill hint");
}

#[test]
fn l3_new_task_modal() {
    let mut app = App::new(".");
    app.modal = Some(Modal::NewTask {
        target: TabId::Orc,
        buffer: "add OAuth".to_string(),
    });
    let output = render_to_string(&mut app, 120, 40);
    assert!(output.contains("new task") || output.contains("task"),
        "should show new task title");
}

#[test]
fn l3_ask_user_modal() {
    let mut app = App::new(".");
    app.modal = Some(Modal::AskUser {
        session_id: "s1".to_string(),
        question_id: "q1".to_string(),
        question: "Which branch to target?".to_string(),
        context: Some("for the PR".to_string()),
        buffer: String::new(),
        hidden: false,
    });
    let output = render_to_string(&mut app, 120, 40);
    assert!(
        output.contains("branch") || output.contains("input"),
        "should show the question or input prompt"
    );
}

#[test]
fn l3_confirm_quit_modal() {
    let mut app = App::new(".");
    app.modal = Some(Modal::ConfirmQuit);
    let output = render_to_string(&mut app, 120, 40);
    assert!(
        output.contains("quit") || output.contains("Quit"),
        "should show quit confirmation"
    );
}

#[test]
fn l3_help_action_bar_uses_h_not_question_mark() {
    // `?` was rebound to open the scratch overlay; help moved to `h`.
    // Verify the action bar hint reflects that (so users notice).
    let mut app = App::new(".");
    let output = render_to_string(&mut app, 120, 40);
    // The action bar lives on the last line; the help entry should
    // pair the `h` key with the `help` label.
    assert!(
        output.contains("h ") && output.contains("help"),
        "action bar should advertise `h help`, got:\n{output}"
    );
    assert!(
        output.contains("? ") && output.contains("scratch"),
        "action bar should advertise `? scratch`, got:\n{output}"
    );
}

#[test]
fn l3_help_modal() {
    let mut app = App::new(".");
    app.modal = Some(Modal::Help);
    let output = render_to_string(&mut app, 120, 40);
    assert!(
        output.contains("help") || output.contains("Help") || output.contains("keybindings"),
        "should show help content"
    );
}

#[test]
#[ignore = "task-graph dashboard removed in v2 spec rewrite"]
fn l3_graph_toggle() {
    let mut app = App::new(".");
    app.add_session(test_session("s1", "worker1", SessionState::Running));
    app.push_chat(ChatRole::Orc, "planned 1 session".to_string());

    // Without graph
    let output_no_graph = render_to_string(&mut app, 120, 40);

    // With graph (field removed in UX overhaul)
    let output_with_graph = render_to_string(&mut app, 120, 40);

    assert!(
        output_with_graph.contains("task graph") || output_with_graph.contains("graph"),
        "should show graph pane when toggled"
    );
    // Graph pane should not appear without toggle
    // (the word "graph" appears in action bar, so check for "task graph" specifically)
    assert!(
        !output_no_graph.contains("task graph"),
        "should not show task graph pane without toggle"
    );
}

#[test]
fn l3_small_terminal_no_panic() {
    let mut app = App::new(".");
    // Very small terminal — should not panic
    let _output = render_to_string(&mut app, 20, 5);
}

#[test]
fn l3_all_session_states_render() {
    let states = vec![
        SessionState::Running,
        SessionState::Blocked {
            kind: BlockKind::Permission,
            reason: "test".to_string(),
        },
        SessionState::Blocked {
            kind: BlockKind::OrcDecision,
            reason: "test".to_string(),
        },
        SessionState::Blocked {
            kind: BlockKind::UserInput,
            reason: "test".to_string(),
        },
        SessionState::AwaitingReview {
            diff_hash: "abc".to_string(),
        },
        SessionState::Done {
            summary: "done".to_string(),
        },
        SessionState::Failed {
            reason: "crashed".to_string(),
        },
    ];

    for (i, state) in states.into_iter().enumerate() {
        let mut app = App::new(".");
        app.add_session(test_session(&format!("s{i}"), &format!("w{i}"), state));
        app.focus_tab(TabId::Worker(0));
        // Should not panic for any state
        let _output = render_to_string(&mut app, 120, 40);
    }
}

// ---------------------------------------------------------------------------
// L3: Review view tests
// ---------------------------------------------------------------------------

fn sample_diff() -> ParsedDiff {
    ParsedDiff {
        files: vec![
            DiffFile {
                path: "src/auth.rs".to_string(),
                hunks: vec![Hunk {
                    id: "h0".to_string(),
                    header: "@@ -10,3 +10,5 @@".to_string(),
                    lines: vec![
                        DiffLine {
                            kind: LineKind::Context,
                            content: "use std::io;".to_string(),
                            old_lineno: Some(10),
                            new_lineno: Some(10),
                        },
                        DiffLine {
                            kind: LineKind::Add,
                            content: "use std::path::Path;".to_string(),
                            old_lineno: None,
                            new_lineno: Some(11),
                        },
                        DiffLine {
                            kind: LineKind::Remove,
                            content: "use std::env;".to_string(),
                            old_lineno: Some(11),
                            new_lineno: None,
                        },
                    ],
                    old_start: 10,
                    new_start: 10,
                }],
                additions: 1,
                deletions: 1,
            },
            DiffFile {
                path: "tests/auth_test.rs".to_string(),
                hunks: vec![Hunk {
                    id: "h0".to_string(),
                    header: "@@ -1,0 +1,5 @@".to_string(),
                    lines: vec![DiffLine {
                        kind: LineKind::Add,
                        content: "fn test_auth() {}".to_string(),
                        old_lineno: None,
                        new_lineno: Some(1),
                    }],
                    old_start: 1,
                    new_start: 1,
                }],
                additions: 1,
                deletions: 0,
            },
        ],
    }
}

#[test]
fn l3_review_view_renders() {
    let mut app = App::new(".");
    app.review = Some(ReviewState::new("s1".to_string(), sample_diff()));
    let output = render_to_string(&mut app, 120, 40);
    assert!(output.contains("src/auth.rs"), "should show file path");
    assert!(
        output.contains("submit") || output.contains("approve"),
        "should show review action hints"
    );
}

#[test]
fn l3_review_with_comments() {
    let mut app = App::new(".");
    let mut review = ReviewState::new("s1".to_string(), sample_diff());
    review.add_comment("why this change?".to_string());
    app.review = Some(review);
    let _output = render_to_string(&mut app, 120, 40);
    // Should not panic
}

#[test]
fn l3_review_empty_diff() {
    let mut app = App::new(".");
    app.review = Some(ReviewState::new(
        "s1".to_string(),
        ParsedDiff { files: vec![] },
    ));
    let _output = render_to_string(&mut app, 120, 40);
    // Should not panic
}
