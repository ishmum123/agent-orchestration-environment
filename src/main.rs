mod app;
mod db;
mod hooks;
mod mcp;
mod orc;
mod policy;
mod review;
mod session;
mod state;
mod tmux;
mod ui;
mod worker;
mod worker_registry;
mod worktree;

use anyhow::{bail, Result};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use app::{App, ChatRole, LogEntry, Modal, TabId};
use db::Database;
use hooks::{HookEvent, HookServer};
use mcp::McpServer;
use orc::{OrcConfig, OrcEvent, OrcProcess};
use policy::PolicyEngine;
use state::{AnsweredBy, StateChange, StateCommand, StateHandle, StateManager};
use worker::WorkerEvent;
use worker_registry::WorkerRegistry;

#[derive(Parser)]
#[command(name = "orc", about = "Parallel Claude Code agents with an intelligent orchestrator")]
struct Cli {
    #[arg(short, long, default_value = ".")]
    project: String,

    #[arg(long, default_value = "opus")]
    model: String,

    #[command(subcommand)]
    command: Option<SubCommand>,
}

#[derive(clap::Subcommand)]
enum SubCommand {
    Doctor,
}

fn check_dependencies() -> Result<()> {
    for dep in ["git", "claude"] {
        let status = std::process::Command::new("which")
            .arg(dep)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => {}
            _ => bail!("required dependency not found: {dep}"),
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(SubCommand::Doctor) = &cli.command {
        return run_doctor().await;
    }

    check_dependencies()?;

    let project_dir = std::fs::canonicalize(&cli.project)?;
    let project_str = project_dir.to_string_lossy().to_string();

    let data_dir = dirs_data_dir().join("orc");
    std::fs::create_dir_all(&data_dir)?;

    let db_path = data_dir.join("state.db");
    let db = Database::open(db_path)?;
    let policy = PolicyEngine::default_policy();
    let (state_handle, state_manager) = StateManager::new(db, policy);
    tokio::spawn(state_manager.run());

    let hook_sock = data_dir.join("hooks.sock");
    let _ = std::fs::remove_file(&hook_sock);
    let (hook_tx, mut hook_rx) = mpsc::channel::<HookEvent>(256);
    let hook_server = HookServer::bind(&hook_sock, hook_tx).await?;
    tokio::spawn(hook_server.run());

    let hook_handle = state_handle.clone();
    tokio::spawn(async move {
        while let Some(event) = hook_rx.recv().await {
            let _ = hook_handle.send(StateCommand::HandleHook { event }).await;
        }
    });

    let (worker_tx, mut worker_rx) = mpsc::unbounded_channel::<WorkerEvent>();
    let worker_registry = WorkerRegistry::new();
    let (orc_inject_tx, mut orc_inject_rx) = mpsc::channel::<String>(64);

    let (mcp_listener, mcp_port) = mcp::bind_http_listener().await?;
    let mcp_server = Arc::new(McpServer::new(
        state_handle.clone(),
        project_dir.clone(),
        hook_sock.clone(),
        mcp_port,
        worker_registry.clone(),
        worker_tx.clone(),
        orc_inject_tx.clone(),
    ));
    mcp::serve_http(mcp_listener, mcp_server);

    let mcp_config_path = orc::write_mcp_config(mcp_port).await?;
    let orc_config = OrcConfig {
        project_dir: project_dir.clone(),
        mcp_config_path,
        model: cli.model.clone(),
    };
    let mut orc_process = orc::spawn_orc(&orc_config).await?;

    let zombies = state::sweep_zombie_sessions(&state_handle).await;
    if zombies > 0 {
        eprintln!("[orc] swept {zombies} zombie session(s) from previous runs");
    }

    let mut app = App::new(&project_str).with_state_handle(state_handle.clone());
    app.push_chat(ChatRole::System, format!("orc v2 — MCP on port {mcp_port}"));

    // Welcome prompt — open a NewTask modal targeted at orc on first frame.
    app.modal = Some(Modal::NewTask {
        target: TabId::Orc,
        buffer: String::new(),
    });

    terminal::enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut state_rx = state_handle.subscribe();

    let result = run_event_loop(
        &mut terminal,
        &mut app,
        &state_handle,
        &mut state_rx,
        &mut orc_process,
        &worker_registry,
        &mut worker_rx,
        &mut orc_inject_rx,
        &project_dir,
        &hook_sock,
        mcp_port,
        worker_tx.clone(),
    )
    .await;

    terminal::disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    let _ = orc_process.kill().await;
    worker_registry.kill_all().await;

    result
}

#[allow(clippy::too_many_arguments)]
async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    state_handle: &StateHandle,
    state_rx: &mut tokio::sync::broadcast::Receiver<StateChange>,
    orc_process: &mut OrcProcess,
    worker_registry: &WorkerRegistry,
    worker_rx: &mut mpsc::UnboundedReceiver<WorkerEvent>,
    orc_inject_rx: &mut mpsc::Receiver<String>,
    project_dir: &PathBuf,
    hook_sock: &PathBuf,
    mcp_port: u16,
    worker_tx: mpsc::UnboundedSender<WorkerEvent>,
) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::render(frame, app))?;

        while event::poll(Duration::ZERO)? {
            if let Event::Key(key) = event::read()? {
                let action = handle_key(key, app, state_handle).await;
                match action {
                    KeyAction::None => {}
                    KeyAction::Quit => return Ok(()),
                    KeyAction::SendToOrc(msg) => {
                        app.push_chat(ChatRole::User, msg.clone());
                        let _ = orc_process.send(&msg).await;
                    }
                    KeyAction::SendToWorker { session_id, body } => {
                        let _ = worker_registry.send(&session_id, &body).await;
                    }
                    KeyAction::InterruptOrc => {
                        let _ = orc_process.interrupt().await;
                        app.orc_view.push(LogEntry::System("interrupted".into()));
                    }
                    KeyAction::InterruptWorker(session_id) => {
                        let _ = worker_registry.interrupt(&session_id).await;
                        app.push_log(&session_id, LogEntry::System("interrupted".into()));
                    }
                    KeyAction::Editor { command, target } => {
                        run_editor(terminal, &command, &target).await?;
                    }
                    KeyAction::RestartWorker { session_id } => {
                        if let Err(e) = restart_worker(
                            app,
                            state_handle,
                            worker_registry,
                            &session_id,
                            project_dir,
                            hook_sock,
                            mcp_port,
                            worker_tx.clone(),
                        )
                        .await
                        {
                            app.push_chat(
                                ChatRole::System,
                                format!("restart failed: {e}"),
                            );
                        }
                    }
                }
            }
        }

        if app.should_quit {
            return Ok(());
        }

        // Poll orc liveness to update the badge.
        app.orc_view.alive = orc_process.is_alive();

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(16)) => {}

            change = state_rx.recv() => {
                if let Ok(change) = change {
                    handle_state_change(app, change);
                }
            }

            Some(ev) = worker_rx.recv() => {
                handle_worker_event(app, state_handle, worker_registry, ev).await;
            }

            Some(msg) = orc_inject_rx.recv() => {
                let _ = orc_process.send(&msg).await;
            }

            events = orc_process.read_events() => {
                if let Ok(events) = events {
                    for event in events {
                        handle_orc_event(app, event);
                    }
                }
            }
        }

        app.tick += 1;
    }
}

async fn handle_worker_event(
    app: &mut App,
    state_handle: &StateHandle,
    worker_registry: &WorkerRegistry,
    ev: WorkerEvent,
) {
    match ev {
        WorkerEvent::Text { session_id, text } => {
            app.push_log(&session_id, LogEntry::AssistantText(text));
        }
        WorkerEvent::Thinking { session_id, text } => {
            app.push_log(&session_id, LogEntry::Thinking(text));
        }
        WorkerEvent::ToolUse {
            session_id,
            name,
            input,
        } => {
            if mcp::is_orc_mcp_tool(&name) {
                if let Some(&idx) = app.session_index.get(&session_id) {
                    app.sessions[idx].skip_next_tool_result = true;
                }
            } else {
                app.push_log(
                    &session_id,
                    LogEntry::ToolUse {
                        name,
                        input_summary: truncate_json(&input),
                    },
                );
            }
        }
        WorkerEvent::ToolResult {
            session_id,
            text,
            is_error,
        } => {
            let skip = if let Some(&idx) = app.session_index.get(&session_id) {
                let s = app.sessions[idx].skip_next_tool_result;
                app.sessions[idx].skip_next_tool_result = false;
                s
            } else {
                false
            };
            if !skip {
                app.push_log(&session_id, LogEntry::ToolResult { text, is_error });
            }
        }
        WorkerEvent::Result {
            session_id,
            cost_usd,
            ..
        } => {
            app.push_log(&session_id, LogEntry::TurnEnd { cost_usd });
        }
        WorkerEvent::ClaudeSessionId {
            session_id,
            claude_session_id,
        } => {
            app.set_claude_session_id(&session_id, claude_session_id.clone());
            let _ = state_handle
                .send(StateCommand::SetClaudeSessionId {
                    session_id,
                    claude_session_id,
                })
                .await;
        }
        WorkerEvent::Exited { session_id, code } => {
            app.push_log(
                &session_id,
                LogEntry::System(format!("worker exited code {code:?}")),
            );
            let _ = worker_registry.kill(&session_id).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

enum KeyAction {
    None,
    Quit,
    SendToOrc(String),
    SendToWorker { session_id: String, body: String },
    InterruptOrc,
    InterruptWorker(String),
    Editor { command: String, target: String },
    RestartWorker { session_id: String },
}

async fn handle_key(key: KeyEvent, app: &mut App, state_handle: &StateHandle) -> KeyAction {
    // Per-tab Ctrl-C interrupts the focused conversation.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        match app.focused_tab {
            TabId::Orc => return KeyAction::InterruptOrc,
            TabId::Worker(idx) => {
                if let Some(sv) = app.sessions.get(idx) {
                    return KeyAction::InterruptWorker(sv.session.id.clone());
                }
            }
        }
        return KeyAction::None;
    }

    if app.modal.is_some() {
        return handle_modal_key(key, app, state_handle).await;
    }

    if app.review.is_some() {
        return handle_review_key(key, app, state_handle).await;
    }

    match key.code {
        KeyCode::Char('q') => {
            if app.sessions.iter().any(|s| {
                matches!(
                    s.session.state,
                    session::SessionState::Running | session::SessionState::Blocked { .. }
                )
            }) {
                app.modal = Some(Modal::ConfirmQuit);
            } else {
                return KeyAction::Quit;
            }
        }
        KeyCode::Char('?') => {
            app.modal = Some(Modal::Help);
        }
        KeyCode::Char('n') => {
            app.modal = Some(Modal::NewTask {
                target: app.focused_tab,
                buffer: String::new(),
            });
        }

        KeyCode::Tab => app.next_tab(),
        KeyCode::BackTab => app.prev_tab(),
        KeyCode::Char(c @ '1'..='9') => {
            let idx = (c as usize) - ('1' as usize);
            if idx == 0 {
                app.focus_tab(TabId::Orc);
            } else if idx - 1 < app.sessions.len() {
                app.focus_tab(TabId::Worker(idx - 1));
            }
        }

        KeyCode::Char('c') => {
            if let TabId::Worker(idx) = app.focused_tab {
                if let Some(sv) = app.sessions.get(idx) {
                    let new_mode = match sv.session.mode {
                        session::SessionMode::Watch => session::SessionMode::Control,
                        session::SessionMode::Control => session::SessionMode::Watch,
                    };
                    let _ = state_handle
                        .send(StateCommand::SetMode {
                            session_id: sv.session.id.clone(),
                            mode: new_mode,
                            reply: tokio::sync::oneshot::channel().0,
                        })
                        .await;
                }
            }
        }
        KeyCode::Char('k') => {
            if let TabId::Worker(idx) = app.focused_tab {
                if let Some(sv) = app.sessions.get(idx) {
                    app.modal = Some(Modal::ConfirmKill {
                        session_id: sv.session.id.clone(),
                        name: sv.session.name.clone(),
                    });
                }
            } else {
                app.scroll_up(app.focused_tab, 1);
            }
        }
        KeyCode::Char('R') => {
            if let TabId::Worker(idx) = app.focused_tab {
                if let Some(sv) = app.sessions.get(idx) {
                    if matches!(sv.session.state, session::SessionState::Failed { .. }) {
                        return KeyAction::RestartWorker {
                            session_id: sv.session.id.clone(),
                        };
                    }
                }
            }
        }
        KeyCode::Char('r') => {
            if let TabId::Worker(idx) = app.focused_tab {
                if let Some(sv) = app.sessions.get(idx) {
                    if let session::SessionState::AwaitingReview { .. } = sv.session.state {
                        let worktree = sv.session.worktree_path.clone();
                        let base = sv.session.base_commit.clone();
                        let sid = sv.session.id.clone();
                        match review::compute_diff(&worktree, &base).await {
                            Ok(diff) => {
                                app.review =
                                    Some(review::ReviewState::with_worktree(sid, diff, worktree));
                            }
                            Err(e) => {
                                app.push_chat(ChatRole::System, format!("diff error: {e}"));
                            }
                        }
                    }
                }
            }
        }

        KeyCode::Down | KeyCode::Char('j') => {
            app.scroll_down(app.focused_tab, 1);
        }
        KeyCode::Up => {
            app.scroll_up(app.focused_tab, 1);
        }
        KeyCode::PageDown => app.scroll_down(app.focused_tab, 10),
        KeyCode::PageUp => app.scroll_up(app.focused_tab, 10),

        _ => {}
    }

    KeyAction::None
}

async fn handle_modal_key(key: KeyEvent, app: &mut App, state_handle: &StateHandle) -> KeyAction {
    let modal = app.modal.take();
    match modal {
        Some(Modal::NewTask { target, mut buffer }) => match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter
                if !buffer.is_empty() && !key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                match target {
                    TabId::Orc => return KeyAction::SendToOrc(buffer),
                    TabId::Worker(idx) => {
                        if let Some(sv) = app.sessions.get(idx) {
                            let id = sv.session.id.clone();
                            let body = buffer.clone();
                            app.push_log(&id, LogEntry::UserText(body.clone()));
                            return KeyAction::SendToWorker {
                                session_id: id,
                                body,
                            };
                        }
                    }
                }
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                buffer.push('\n');
                app.modal = Some(Modal::NewTask { target, buffer });
            }
            KeyCode::Char(c) => {
                buffer.push(c);
                app.modal = Some(Modal::NewTask { target, buffer });
            }
            KeyCode::Backspace => {
                buffer.pop();
                app.modal = Some(Modal::NewTask { target, buffer });
            }
            _ => {
                app.modal = Some(Modal::NewTask { target, buffer });
            }
        },
        Some(Modal::AskUser {
            session_id,
            question_id,
            question,
            context,
            mut buffer,
        }) => match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter if !buffer.is_empty() => {
                let _ = state_handle.answer_user(&session_id, &buffer).await;
            }
            KeyCode::Char(c) => {
                buffer.push(c);
                app.modal = Some(Modal::AskUser {
                    session_id,
                    question_id,
                    question,
                    context,
                    buffer,
                });
            }
            KeyCode::Backspace => {
                buffer.pop();
                app.modal = Some(Modal::AskUser {
                    session_id,
                    question_id,
                    question,
                    context,
                    buffer,
                });
            }
            _ => {
                app.modal = Some(Modal::AskUser {
                    session_id,
                    question_id,
                    question,
                    context,
                    buffer,
                });
            }
        },
        Some(Modal::Comment {
            session_id,
            file,
            line,
            mut buffer,
        }) => match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter if !buffer.is_empty() => {
                if let Some(rev) = app.review.as_mut() {
                    rev.add_comment_at(file, line, buffer);
                }
            }
            KeyCode::Char(c) => {
                buffer.push(c);
                app.modal = Some(Modal::Comment {
                    session_id,
                    file,
                    line,
                    buffer,
                });
            }
            KeyCode::Backspace => {
                buffer.pop();
                app.modal = Some(Modal::Comment {
                    session_id,
                    file,
                    line,
                    buffer,
                });
            }
            _ => {
                app.modal = Some(Modal::Comment {
                    session_id,
                    file,
                    line,
                    buffer,
                });
            }
        },
        Some(Modal::ConfirmKill { session_id, name }) => match key.code {
            KeyCode::Char('y') => {
                state_handle
                    .send(StateCommand::RemoveSession {
                        session_id: session_id.clone(),
                    })
                    .await
                    .ok();
                app.remove_session(&session_id);
                let _ = app; // silence unused binding warning
                let _ = name;
            }
            KeyCode::Esc | KeyCode::Char('n') => {}
            _ => {
                app.modal = Some(Modal::ConfirmKill { session_id, name });
            }
        },
        Some(Modal::ConfirmQuit) => match key.code {
            KeyCode::Char('y') => return KeyAction::Quit,
            KeyCode::Esc | KeyCode::Char('n') => {}
            _ => {
                app.modal = Some(Modal::ConfirmQuit);
            }
        },
        Some(Modal::Help) => {
            // Any key dismisses
        }
        None => {}
    }
    KeyAction::None
}

async fn handle_review_key(
    key: KeyEvent,
    app: &mut App,
    state_handle: &StateHandle,
) -> KeyAction {
    let review = match app.review.as_mut() {
        Some(r) => r,
        None => return KeyAction::None,
    };

    match key.code {
        KeyCode::Char('j') | KeyCode::Down => review.move_line_down(),
        KeyCode::Char('k') | KeyCode::Up => review.move_line_up(),
        KeyCode::Char('J') => review.move_hunk_down(),
        KeyCode::Char('K') => review.move_hunk_up(),

        KeyCode::Char('a') => {
            review.toggle_hunk_approval();
        }

        KeyCode::Char('o') => {
            review.toggle_view();
        }

        KeyCode::Char('e') => {
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
            let target = review
                .current_file()
                .map(|f| {
                    if review.worktree_path.is_empty() {
                        f.path.clone()
                    } else {
                        format!("{}/{}", review.worktree_path, f.path)
                    }
                })
                .unwrap_or_else(|| review.worktree_path.clone());
            return KeyAction::Editor {
                command: editor,
                target,
            };
        }

        KeyCode::Char('c') => {
            if let Some(file) = review.current_file().cloned() {
                let line = review
                    .current_line()
                    .and_then(|l| l.new_lineno)
                    .unwrap_or(0);
                let session_id = review.session_id.clone();
                app.modal = Some(Modal::Comment {
                    session_id,
                    file: file.path,
                    line,
                    buffer: String::new(),
                });
            }
        }

        KeyCode::Char('s') => {
            let payload = review.to_payload();
            let session_id = review.session_id.clone();
            let payload_json = serde_json::to_string_pretty(&payload).unwrap_or_default();

            if let Some(sv) = app.sessions.iter().find(|s| s.session.id == session_id) {
                let _name = sv.session.name.clone();
                // Worker delivery via WorkerRegistry happens at call site; here
                // we just push the review feedback as a system note + apply.
                app.push_chat(
                    ChatRole::System,
                    format!("review submitted for {}", sv.session.name),
                );
            }

            let _ = state_handle
                .send(StateCommand::ApplyEvent {
                    session_id,
                    event: session::SessionEvent::ReviewSubmitted {
                        approved: true,
                        feedback: Some(payload_json),
                    },
                    reply: tokio::sync::oneshot::channel().0,
                })
                .await;

            app.review = None;
        }

        KeyCode::Char('q') | KeyCode::Esc => {
            app.review = None;
        }

        _ => {}
    }

    KeyAction::None
}

fn handle_state_change(app: &mut App, change: StateChange) {
    match change {
        StateChange::SessionCreated { session } => {
            if !app.session_index.contains_key(&session.id) {
                let name = session.name.clone();
                app.add_session(session);
                app.push_chat(ChatRole::System, format!("session '{name}' spawned"));
            }
        }
        StateChange::SessionStateChanged {
            session_id,
            old: _,
            new_state,
        } => {
            let label = state::state_label(&new_state);
            app.update_session_state(&session_id, new_state);
            let prefix = if session_id.len() >= 8 {
                &session_id[..8]
            } else {
                &session_id
            };
            app.push_chat(ChatRole::System, format!("session {prefix}: {label}"));
        }
        StateChange::SessionModeChanged { session_id, mode } => {
            app.update_session_mode(&session_id, mode);
        }
        StateChange::SessionRemoved { session_id } => {
            app.remove_session(&session_id);
        }
        StateChange::UserQuestionPending {
            session_id,
            question_id,
            question,
            context,
        } => {
            app.modal = Some(Modal::AskUser {
                session_id,
                question_id,
                question,
                context,
                buffer: String::new(),
            });
        }
        StateChange::QuestionResolved {
            session_id,
            question_id,
            answered_by,
        } => {
            if let Some(Modal::AskUser {
                question_id: open_id,
                ..
            }) = &app.modal
            {
                if open_id == &question_id {
                    app.modal = None;
                }
            }
            let by = match answered_by {
                AnsweredBy::User => "you",
                AnsweredBy::Orc => "orc",
            };
            app.push_log(&session_id, LogEntry::System(format!("answered by {by}")));
        }
        StateChange::PermissionNeeded {
            session_id,
            request,
        } => {
            let prefix = if session_id.len() >= 8 {
                &session_id[..8]
            } else {
                &session_id
            };
            app.push_chat(
                ChatRole::System,
                format!("permission escalated: {} for {prefix}", request.tool_name),
            );
        }
        StateChange::TaskGraphUpdated { .. } => {}
        StateChange::HookReceived { .. } => {}
        StateChange::SummaryUpdated {
            session_id,
            summary,
        } => {
            app.set_session_summary(session_id, summary);
        }
    }
}

fn handle_orc_event(app: &mut App, event: OrcEvent) {
    match event {
        OrcEvent::Text(text) => {
            app.orc_view.push(LogEntry::AssistantText(text));
        }
        OrcEvent::ToolUse { name, input, .. } => {
            if mcp::is_orc_mcp_tool(&name) {
                app.orc_view.skip_next_tool_result = true;
            } else {
                app.orc_view.push(LogEntry::ToolUse {
                    name,
                    input_summary: truncate_json(&input),
                });
            }
        }
        OrcEvent::Result {
            is_error,
            result,
            cost_usd,
            ..
        } => {
            if is_error {
                app.orc_view
                    .push(LogEntry::System(format!("orc error: {result}")));
            }
            app.orc_view.push(LogEntry::TurnEnd { cost_usd });
        }
        OrcEvent::System { model, .. } => {
            if let Some(m) = model {
                app.orc_view
                    .push(LogEntry::System(format!("orc model: {m}")));
            }
        }
        OrcEvent::Thinking(t) => {
            app.orc_view.push(LogEntry::Thinking(t));
        }
    }
}

fn truncate_json(v: &serde_json::Value) -> String {
    let s = v.to_string();
    if s.len() > 80 {
        format!("{}...", &s[..77])
    } else {
        s
    }
}

/// Run an external editor on a file path. Surrender alt-screen, run blocking,
/// then re-enter. Mirrors the previous tmux-attach dance.
async fn run_editor(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    command: &str,
    target: &str,
) -> Result<()> {
    terminal::disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    let status = std::process::Command::new(command).arg(target).status();

    io::stdout().execute(EnterAlternateScreen)?;
    terminal::enable_raw_mode()?;
    terminal.clear()?;

    if let Err(e) = status {
        eprintln!("[orc] editor error: {e}");
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn restart_worker(
    app: &mut App,
    state_handle: &StateHandle,
    worker_registry: &WorkerRegistry,
    session_id: &str,
    project_dir: &std::path::Path,
    hook_sock: &std::path::Path,
    mcp_port: u16,
    worker_tx: mpsc::UnboundedSender<WorkerEvent>,
) -> Result<()> {
    let session = state_handle
        .get_session(session_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("session not found"))?;

    // Drop any existing handle (no-op if absent).
    let _ = worker_registry.kill(session_id).await;

    // Build MCP config + hook relay (mirror tool_spawn_session shape).
    let mcp_cfg = mcp::generate_mcp_config(mcp_port);
    let mcp_cfg_path = std::env::temp_dir()
        .join(format!("orc-worker-mcp-{}.json", session.name));
    let _ = tokio::fs::write(
        &mcp_cfg_path,
        serde_json::to_string_pretty(&mcp_cfg).unwrap_or_default(),
    )
    .await;
    let tmux_name = format!("orc-{}", session.name);
    let _ = hooks::create_hook_script(hook_sock, &tmux_name).await;

    let prompt = mcp::worker_system_prompt(&session.id, &session.name, &session.task);
    let worktree = std::path::PathBuf::from(&session.worktree_path);

    let handle = if let Some(claude_sid) = session.claude_session_id.clone() {
        worker::spawn_worker_resume(
            session.id.clone(),
            worktree,
            session.model.clone(),
            mcp_cfg_path,
            prompt,
            claude_sid,
            worker_tx,
        )
        .await?
    } else {
        worker::spawn_worker(
            session.id.clone(),
            worktree,
            session.model.clone(),
            mcp_cfg_path,
            prompt,
            session.task.clone(),
            worker_tx,
        )
        .await?
    };
    worker_registry.insert(handle).await;

    state_handle
        .apply_event(session_id, session::SessionEvent::Restarted)
        .await?;

    let _ = (project_dir, app);
    Ok(())
}

async fn run_doctor() -> Result<()> {
    println!("orc doctor — checking environment\n");
    let mut all_ok = true;

    match std::process::Command::new("git").arg("--version").output() {
        Ok(o) if o.status.success() => {
            let ver = String::from_utf8_lossy(&o.stdout);
            println!("\u{2713} git: {}", ver.trim());
        }
        _ => {
            println!("\u{2717} git: not found or not functional");
            all_ok = false;
        }
    }

    match std::process::Command::new("claude").arg("--version").output() {
        Ok(o) if o.status.success() => {
            let ver = String::from_utf8_lossy(&o.stdout);
            println!("\u{2713} claude: {}", ver.trim());
        }
        _ => {
            println!("\u{2717} claude: not found or not functional");
            all_ok = false;
        }
    }

    let data_dir = dirs_data_dir().join("orc");
    if data_dir.exists() {
        let test_file = data_dir.join(".doctor-probe");
        match std::fs::write(&test_file, "probe") {
            Ok(_) => {
                let _ = std::fs::remove_file(&test_file);
                println!("\u{2713} data directory: {} (writable)", data_dir.display());
            }
            Err(e) => {
                println!(
                    "\u{2717} data directory: {} (not writable: {e})",
                    data_dir.display()
                );
                all_ok = false;
            }
        }
    } else {
        println!(
            "\u{2717} data directory: {} (does not exist)",
            data_dir.display()
        );
        all_ok = false;
    }

    let db_path = data_dir.join("state.db");
    match Database::open(&db_path) {
        Ok(_) => {
            println!("\u{2713} database: {} (openable)", db_path.display());
        }
        Err(e) => {
            println!("\u{2717} database: {} (error: {e})", db_path.display());
            all_ok = false;
        }
    }

    println!();
    if all_ok {
        println!("all checks passed");
    } else {
        println!("some checks failed — see above");
    }

    Ok(())
}

fn dirs_data_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".config")
    } else {
        PathBuf::from(".")
    }
}
