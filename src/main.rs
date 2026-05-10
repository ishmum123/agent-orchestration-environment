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
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent,
    KeyModifiers,
};
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
    /// Remove finished (Done/Failed) sessions from the local sqlite state.
    Clean,
}

fn check_dependencies() -> Result<()> {
    let claude = worker::claude_bin();
    for dep in ["git", claude.as_str()] {
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
    if let Some(SubCommand::Clean) = &cli.command {
        return run_clean().await;
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
    app.orc_view.model = orc_config.model.clone();
    app.push_chat(ChatRole::System, format!("orc v2 — MCP on port {mcp_port}"));

    // Welcome prompt — open a NewTask modal targeted at orc on first frame.
    app.modal = Some(Modal::NewTask {
        target: TabId::Orc,
        buffer: String::new(),
    });

    // Install a panic hook that restores terminal state before printing
    // the panic info. Without this, a panic leaves alt-screen + raw mode
    // + mouse capture on and corrupts the user's shell.
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = io::stdout();
        let _ = out.execute(DisableBracketedPaste);
        let _ = terminal::disable_raw_mode();
        let _ = out.execute(LeaveAlternateScreen);
        // Best-effort: persist a copy of the panic to a file so users have
        // something to share even if the terminal scrollback is gone.
        if let Some(home) = std::env::var_os("HOME") {
            let p = std::path::PathBuf::from(home).join(".config/orc/last-panic.log");
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(
                &p,
                format!("{info}\n\nbacktrace:\n{}\n", std::backtrace::Backtrace::force_capture()),
            );
        }
        default_panic(info);
    }));

    // Redirect stderr to a log file before entering the alt-screen. While
    // the TUI is up, raw eprintln! bytes from anywhere in the process
    // (state.rs, mcp.rs, hooks.rs, worker.rs, panic handler tail-prints,
    // etc.) would otherwise land on top of the ratatui buffer and leave
    // stray characters that bleed across tab switches.
    let stderr_log = data_dir.join("stderr.log");
    {
        use std::os::unix::io::AsRawFd;
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&stderr_log)
        {
            // Safety: dup2 with valid fds; we keep `file` alive only long
            // enough to copy the descriptor, then drop it. The duplicated
            // fd remains as the new stderr.
            unsafe {
                libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO);
            }
        }
    }

    terminal::enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    io::stdout().execute(EnableBracketedPaste)?;
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

    let _ = io::stdout().execute(DisableBracketedPaste);
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
        // (App passed mutably so render() can pin sticky-bottom scroll.)

        while event::poll(Duration::ZERO)? {
            let evt = event::read()?;
            if let Event::Paste(text) = &evt {
                handle_paste(text, app);
                continue;
            }
            if let Event::Key(key) = evt {
                let action = handle_key(key, app, state_handle).await;
                match action {
                    KeyAction::None => {}
                    KeyAction::Quit => return Ok(()),
                    KeyAction::SendToOrc(msg) => {
                        app.push_chat(ChatRole::User, msg.clone());
                        cleanup_idle_done_sessions(
                            app,
                            state_handle,
                            worker_registry,
                            project_dir,
                            &msg,
                        )
                        .await;
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
                    KeyAction::Editor { command, target, line } => {
                        run_editor(terminal, &command, &target, line).await?;
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
        // Mirror orc's latest-turn context occupancy into the view.
        let last_ctx = orc_process.usage().last_context_tokens;
        app.orc_view.last_context_tokens = if last_ctx > 0 { Some(last_ctx) } else { None };

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
            clear_thinking(app, &session_id);
            app.push_log(&session_id, LogEntry::AssistantText(text));
        }
        WorkerEvent::Thinking { session_id, text: _ } => {
            // Don't show the text; just flip the spinner on. Cleared by
            // the next visible event (text / tool / turn-end).
            if let Some(&idx) = app.session_index.get(&session_id) {
                app.sessions[idx].is_thinking = true;
            }
        }
        WorkerEvent::ToolUse {
            session_id,
            name,
            input,
        } => {
            clear_thinking(app, &session_id);
            if mcp::should_hide_tool(&name) {
                if let Some(&idx) = app.session_index.get(&session_id) {
                    app.sessions[idx].skip_next_tool_result = true;
                }
            } else {
                let input_summary = ui::worker::summarize_tool_input(&name, &input);
                app.push_log(
                    &session_id,
                    LogEntry::ToolUse {
                        name,
                        input_summary,
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
            clear_thinking(app, &session_id);
            app.push_log(&session_id, LogEntry::TurnEnd { cost_usd });
            if let Some(&idx) = app.session_index.get(&session_id) {
                app.collapse_last_turn(TabId::Worker(idx));
            }
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
        WorkerEvent::OrcInstruction { session_id, text } => {
            app.push_log(&session_id, LogEntry::OrcInstruction(text));
        }
        WorkerEvent::Usage {
            session_id,
            context_tokens,
        } => {
            if let Some(&idx) = app.session_index.get(&session_id) {
                app.sessions[idx].last_context_tokens = Some(context_tokens);
            }
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
    Editor { command: String, target: String, line: Option<usize> },
    RestartWorker { session_id: String },
}

/// Clear the spinner flag for a worker once it produces visible output.
fn clear_thinking(app: &mut App, session_id: &str) {
    if let Some(&idx) = app.session_index.get(session_id) {
        app.sessions[idx].is_thinking = false;
    }
}

/// Append a pasted block to the active modal's text buffer. If no input
/// modal is open, the paste is dropped (orc has no persistent input box).
fn handle_paste(text: &str, app: &mut App) {
    let modal = app.modal.take();
    let new = match modal {
        Some(Modal::NewTask { target, mut buffer }) => {
            buffer.push_str(text);
            Some(Modal::NewTask { target, buffer })
        }
        Some(Modal::AskUser {
            session_id,
            question_id,
            question,
            context,
            mut buffer,
            hidden,
        }) => {
            if !hidden {
                buffer.push_str(text);
            }
            Some(Modal::AskUser {
                session_id,
                question_id,
                question,
                context,
                buffer,
                hidden,
            })
        }
        Some(Modal::Comment {
            session_id,
            file,
            line,
            mut buffer,
        }) => {
            buffer.push_str(text);
            Some(Modal::Comment {
                session_id,
                file,
                line,
                buffer,
            })
        }
        other => other,
    };
    app.modal = new;
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
        KeyCode::Char('t') => {
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
        KeyCode::Char('x') => {
            if let TabId::Worker(idx) = app.focused_tab {
                if let Some(sv) = app.sessions.get(idx) {
                    app.modal = Some(Modal::ConfirmKill {
                        session_id: sv.session.id.clone(),
                        name: sv.session.name.clone(),
                    });
                }
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

        KeyCode::Down | KeyCode::Char('j') | KeyCode::PageDown => {
            let amount = if matches!(key.code, KeyCode::PageDown) { 10 } else { 1 };
            app.scroll_down(app.focused_tab, amount);
            // Don't change stick here. Autoscroll re-engages stick if the
            // user's downward scroll reaches the bottom.
        }
        KeyCode::Up | KeyCode::Char('k') | KeyCode::PageUp => {
            let amount = if matches!(key.code, KeyCode::PageUp) { 10 } else { 1 };
            app.scroll_up(app.focused_tab, amount);
            app.set_stick(app.focused_tab, false);
        }
        KeyCode::Home => {
            app.set_scroll(app.focused_tab, 0);
            app.set_stick(app.focused_tab, false);
        }
        KeyCode::End | KeyCode::Char('G') => {
            app.set_stick(app.focused_tab, true);
        }
        KeyCode::Char('g') => {
            // `gg` chord: first `g` arms, second jumps to top.
            if app.pending_g {
                app.set_scroll(app.focused_tab, 0);
                app.set_stick(app.focused_tab, false);
                app.pending_g = false;
            } else {
                app.pending_g = true;
                return KeyAction::None;
            }
        }

        _ => {}
    }

    // Clear `gg` chord on any key other than `g`.
    if !matches!(key.code, KeyCode::Char('g')) {
        app.pending_g = false;
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
            hidden,
        }) => {
            // Tab toggles modal visibility so the user can peek the chat.
            if matches!(key.code, KeyCode::Tab) {
                app.modal = Some(Modal::AskUser {
                    session_id,
                    question_id,
                    question,
                    context,
                    buffer,
                    hidden: !hidden,
                });
                return KeyAction::None;
            }
            // While hidden, route scroll keys to chat; ignore other keys
            // (so Enter/letters can't accidentally answer the question).
            if hidden {
                let restore = || Modal::AskUser {
                    session_id: session_id.clone(),
                    question_id: question_id.clone(),
                    question: question.clone(),
                    context: context.clone(),
                    buffer: buffer.clone(),
                    hidden: true,
                };
                match key.code {
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.scroll_down(app.focused_tab, 1);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.scroll_up(app.focused_tab, 1);
                        app.set_stick(app.focused_tab, false);
                    }
                    KeyCode::PageDown => {
                        app.scroll_down(app.focused_tab, 10);
                    }
                    KeyCode::PageUp => {
                        app.scroll_up(app.focused_tab, 10);
                        app.set_stick(app.focused_tab, false);
                    }
                    KeyCode::Char('G') | KeyCode::End => {
                        app.set_stick(app.focused_tab, true);
                    }
                    KeyCode::Home => {
                        app.set_scroll(app.focused_tab, 0);
                        app.set_stick(app.focused_tab, false);
                    }
                    _ => {}
                }
                app.modal = Some(restore());
                return KeyAction::None;
            }
            match key.code {
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
                        hidden,
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
                        hidden,
                    });
                }
                _ => {
                    app.modal = Some(Modal::AskUser {
                        session_id,
                        question_id,
                        question,
                        context,
                        buffer,
                        hidden,
                    });
                }
            }
        }
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
        KeyCode::Char('J') => match review.view_mode {
            crate::review::ViewMode::Diff => review.move_hunk_down(),
            crate::review::ViewMode::WholeFile => review.whole_file_page(15),
        },
        KeyCode::Char('K') => match review.view_mode {
            crate::review::ViewMode::Diff => review.move_hunk_up(),
            crate::review::ViewMode::WholeFile => review.whole_file_page(-15),
        },
        KeyCode::Char(']') => review.move_file_down(),
        KeyCode::Char('[') => review.move_file_up(),

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
            let line = review
                .current_line()
                .and_then(|l| l.new_lineno.or(l.old_lineno));
            return KeyAction::Editor {
                command: editor,
                target,
                line,
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
                let id = session.id.clone();
                let task = session.task.clone();
                app.add_session(session);
                app.push_chat(ChatRole::System, format!("session '{name}' spawned"));
                // Show the initial task in the worker's own log so the user
                // can see what orc told this worker to do.
                app.push_log(&id, LogEntry::OrcInstruction(task));
            }
        }
        StateChange::SessionStateChanged {
            session_id,
            old: _,
            new_state,
        } => {
            // AwaitingReview gets its own friendly line via
            // WorkerReviewSubmitted (which carries the summary). Skip the
            // raw badge here so the user only sees one notification.
            let suppress = matches!(&new_state, session::SessionState::AwaitingReview { .. });
            let name = app.session_index.get(&session_id).and_then(|&i| {
                app.sessions.get(i).map(|s| s.session.name.clone())
            });
            app.update_session_state(&session_id, new_state.clone());
            if !suppress {
                let pretty = match &new_state {
                    session::SessionState::Done { .. } => "done".to_string(),
                    session::SessionState::Failed { .. } => "failed".to_string(),
                    session::SessionState::Blocked { .. } => "blocked".to_string(),
                    session::SessionState::Running => "running".to_string(),
                    other => state::state_label(other).to_lowercase(),
                };
                let who = name.unwrap_or_else(|| {
                    if session_id.len() >= 8 {
                        session_id[..8].to_string()
                    } else {
                        session_id.clone()
                    }
                });
                app.push_chat(ChatRole::System, format!("{who}: {pretty}"));
            }
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
                hidden: false,
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
        StateChange::WorkerReviewSubmitted { name, summary, .. } => {
            let line = if summary.is_empty() {
                format!("{name} ready for review")
            } else {
                format!("{name} ready for review: {summary}")
            };
            app.push_chat(ChatRole::System, line);
        }
    }
}

fn handle_orc_event(app: &mut App, event: OrcEvent) {
    match event {
        OrcEvent::Text(text) => {
            app.orc_view.is_thinking = false;
            app.orc_view.push(LogEntry::AssistantText(text));
        }
        OrcEvent::ToolUse { name, input, .. } => {
            app.orc_view.is_thinking = false;
            if mcp::should_hide_tool(&name) {
                app.orc_view.skip_next_tool_result = true;
            } else {
                let input_summary = ui::worker::summarize_tool_input(&name, &input);
                app.orc_view.push(LogEntry::ToolUse {
                    name,
                    input_summary,
                });
            }
        }
        OrcEvent::Result {
            is_error,
            result,
            cost_usd,
            ..
        } => {
            app.orc_view.is_thinking = false;
            if is_error {
                app.orc_view
                    .push(LogEntry::System(format!("orc error: {result}")));
            }
            app.orc_view.push(LogEntry::TurnEnd { cost_usd });
            app.collapse_last_turn(TabId::Orc);
        }
        OrcEvent::System { model, .. } => {
            if let Some(m) = model {
                if !app.orc_view.model_announced {
                    app.orc_view
                        .push(LogEntry::System(format!("orc model: {m}")));
                    app.orc_view.model_announced = true;
                }
            }
        }
        OrcEvent::Thinking(_) => {
            // Hide the text; show only the spinner.
            app.orc_view.is_thinking = true;
        }
    }
}

/// Auto-cleanup idle Done sessions when the user moves on. Triggered on
/// each new orc-bound message: any session that has been Done for ≥ 5
/// minutes and whose name is not mentioned in the new message is killed,
/// its worktree pruned, and its row removed from sqlite. The session's
/// summary already lives in the orc-tab event log, so the user keeps the
/// receipt without the dead tab.
async fn cleanup_idle_done_sessions(
    app: &mut App,
    state_handle: &StateHandle,
    workers: &WorkerRegistry,
    project_dir: &PathBuf,
    user_msg: &str,
) {
    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::minutes(5);
    let msg_lower = user_msg.to_ascii_lowercase();

    let candidates: Vec<(String, String, String)> = app
        .sessions
        .iter()
        .filter_map(|sv| {
            let is_done = matches!(sv.session.state, session::SessionState::Done { .. });
            if !is_done {
                return None;
            }
            let ended = sv.session.ended_at?;
            if ended > cutoff {
                return None;
            }
            let name_lower = sv.session.name.to_ascii_lowercase();
            if !name_lower.is_empty() && msg_lower.contains(&name_lower) {
                return None;
            }
            Some((
                sv.session.id.clone(),
                sv.session.name.clone(),
                sv.session.worktree_path.clone(),
            ))
        })
        .collect();

    for (id, name, worktree) in candidates {
        let _ = workers.kill(&id).await;
        if !worktree.is_empty() {
            let _ = crate::worktree::remove_worktree(
                project_dir.to_str().unwrap_or("."),
                std::path::Path::new(&worktree),
                &name,
            )
            .await;
        }
        let _ = state_handle
            .send(StateCommand::RemoveSession {
                session_id: id.clone(),
            })
            .await;
        app.orc_view
            .push(LogEntry::System(format!("{name} cleaned up (idle > 5m)")));
    }
}

/// Run an external editor on a file path. Surrender alt-screen, run blocking,
/// then re-enter. Mirrors the previous tmux-attach dance.
async fn run_editor(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    command: &str,
    target: &str,
    line: Option<usize>,
) -> Result<()> {
    terminal::disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    let status = {
        let mut cmd = std::process::Command::new(command);
        // Detect editor family from the first path segment so we pass the
        // right "go to line" flag. Defaults to the `+N file` convention,
        // which vi/vim/nano/emacs accept.
        let bin = std::path::Path::new(command)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(command);
        match (bin, line) {
            (_, None) => {
                cmd.arg(target);
            }
            ("code" | "code-insiders" | "cursor" | "windsurf", Some(n)) => {
                cmd.arg("--goto").arg(format!("{target}:{n}"));
            }
            ("subl" | "sublime_text" | "atom" | "zed", Some(n)) => {
                cmd.arg(format!("{target}:{n}"));
            }
            (_, Some(n)) => {
                cmd.arg(format!("+{n}")).arg(target);
            }
        }
        cmd.status()
    };

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

async fn run_clean() -> Result<()> {
    let data_dir = dirs_data_dir().join("orc");
    let db_path = data_dir.join("state.db");
    if !db_path.exists() {
        println!("no state db at {} — nothing to clean", db_path.display());
        return Ok(());
    }
    let db = Database::open(&db_path)?;
    let removed = db.delete_finished_sessions()?;
    if removed == 0 {
        println!("no finished sessions found");
    } else {
        println!("removed {removed} finished session(s) from {}", db_path.display());
    }
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

    match std::process::Command::new(worker::claude_bin()).arg("--version").output() {
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
