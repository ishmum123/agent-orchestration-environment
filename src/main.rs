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
mod worktree;

use anyhow::{bail, Result};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{
    self, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use app::{App, ChatRole, Modal, TabId};
use review::ReviewState;
use db::Database;
use hooks::{HookEvent, HookServer};
use mcp::McpServer;
use orc::{OrcConfig, OrcEvent, OrcProcess};
use policy::PolicyEngine;
use state::{StateChange, StateCommand, StateHandle, StateManager};

#[derive(Parser)]
#[command(name = "orc", about = "Parallel Claude Code agents with an intelligent orchestrator")]
struct Cli {
    /// Project directory
    #[arg(short, long, default_value = ".")]
    project: String,

    /// Model for the orc brain (default: opus)
    #[arg(long, default_value = "opus")]
    model: String,

    #[command(subcommand)]
    command: Option<SubCommand>,
}

#[derive(clap::Subcommand)]
enum SubCommand {
    /// Check system dependencies and environment health
    Doctor,
}

fn check_dependencies() -> Result<()> {
    for dep in ["git", "claude", "tmux"] {
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

    // -- Data directory -------------------------------------------------------
    let data_dir = dirs_data_dir().join("orc");
    std::fs::create_dir_all(&data_dir)?;

    // -- State manager --------------------------------------------------------
    let db_path = data_dir.join("state.db");
    let db = Database::open(db_path)?;
    let policy = PolicyEngine::default_policy();
    let (state_handle, state_manager) = StateManager::new(db, policy);
    tokio::spawn(state_manager.run());

    // -- Hook server ----------------------------------------------------------
    let hook_sock = data_dir.join("hooks.sock");
    let _ = std::fs::remove_file(&hook_sock); // clean up stale socket
    let (hook_tx, mut hook_rx) = mpsc::channel::<HookEvent>(256);
    let hook_server = HookServer::bind(&hook_sock, hook_tx).await?;
    tokio::spawn(hook_server.run());

    // Forward hook events into state manager
    let hook_handle = state_handle.clone();
    tokio::spawn(async move {
        while let Some(event) = hook_rx.recv().await {
            let _ = hook_handle
                .send(StateCommand::HandleHook { event })
                .await;
        }
    });

    // -- MCP HTTP server ------------------------------------------------------
    let (mcp_listener, mcp_port) = mcp::bind_http_listener().await?;
    let mcp_server = Arc::new(McpServer::new(
        state_handle.clone(),
        project_dir.clone(),
        hook_sock.clone(),
        mcp_port,
    ));
    mcp::serve_http(mcp_listener, mcp_server);

    // -- Orc brain process ----------------------------------------------------
    let mcp_config_path = orc::write_mcp_config(mcp_port).await?;
    let orc_config = OrcConfig {
        project_dir: project_dir.clone(),
        mcp_config_path,
        model: cli.model.clone(),
    };
    let mut orc_process = orc::spawn_orc(&orc_config).await?;

    // -- Cleanup orphaned tmux sessions from previous runs --------------------
    let cleaned = tmux::cleanup_orphaned().await.unwrap_or(0);
    if cleaned > 0 {
        eprintln!("[orc] cleaned up {cleaned} orphaned tmux sessions");
    }

    // -- Sweep zombie sessions: anything in DB whose tmux is gone is dead.
    //    Transitions Running/AwaitingReview → Failed (so `R` restart works);
    //    Blocked → removed outright (state machine forbids Blocked→Failed).
    let zombies = state::sweep_zombie_sessions(&state_handle).await;
    if zombies > 0 {
        eprintln!("[orc] swept {zombies} zombie session(s) from previous runs");
    }

    // -- App state ------------------------------------------------------------
    let mut app = App::new(&project_str).with_state_handle(state_handle.clone());
    app.push_chat(ChatRole::System, format!("orc v2 — MCP on port {mcp_port}"));

    // -- Terminal setup -------------------------------------------------------
    terminal::enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // -- State broadcast subscription ----------------------------------------
    let mut state_rx = state_handle.subscribe();

    // -- PTY tail channel: each event is a full snapshot of the worker's
    //    visible pane (replaces the tail buffer). Captured every ~1s.
    let (pty_tx, mut pty_rx) = mpsc::channel::<(String, Vec<String>)>(64);

    // -- Main event loop ------------------------------------------------------
    let result = run_event_loop(
        &mut terminal,
        &mut app,
        &state_handle,
        &mut state_rx,
        &mut orc_process,
        pty_tx,
        &mut pty_rx,
    )
    .await;

    // -- Cleanup --------------------------------------------------------------
    terminal::disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    // Kill orc brain
    let _ = orc_process.kill().await;

    // Kill all worker tmux sessions
    let _ = tmux::cleanup_orphaned().await;

    result
}

async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    state_handle: &StateHandle,
    state_rx: &mut tokio::sync::broadcast::Receiver<StateChange>,
    orc_process: &mut OrcProcess,
    pty_tx: mpsc::Sender<(String, Vec<String>)>,
    pty_rx: &mut mpsc::Receiver<(String, Vec<String>)>,
) -> Result<()> {
    loop {
        // Render
        terminal.draw(|frame| ui::render(frame, app))?;

        // Always drain crossterm events first (non-blocking) so keys are
        // never starved by fast-completing async branches in select!.
        while event::poll(Duration::ZERO)? {
            if let Event::Key(key) = event::read()? {
                let action = handle_key(key, app, state_handle).await;
                match action {
                    KeyAction::None => {}
                    KeyAction::Quit => return Ok(()),
                    KeyAction::Attach(session_name) => {
                        do_attach(terminal, &session_name).await?;
                    }
                    KeyAction::SendToOrc(msg) => {
                        app.push_chat(ChatRole::User, msg.clone());
                        let _ = orc_process.send(&msg).await;
                    }
                }
            }
        }

        if app.should_quit {
            return Ok(());
        }

        // Wait for async events or a frame tick (~60fps).
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(16)) => {}

            change = state_rx.recv() => {
                if let Ok(change) = change {
                    if let StateChange::SessionCreated { session } = &change {
                        tmux::spawn_pane_capturer(
                            session.id.clone(),
                            session.tmux_session.clone(),
                            pty_tx.clone(),
                            Duration::from_millis(1000),
                        );
                    }
                    handle_state_change(app, change);
                }
            }

            Some((session_id, lines)) = pty_rx.recv() => {
                app.set_pty_tail(&session_id, lines);
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

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

enum KeyAction {
    None,
    Quit,
    Attach(String),          // tmux session name to attach to
    SendToOrc(String),       // message to send to orc brain
}

async fn handle_key(key: KeyEvent, app: &mut App, state_handle: &StateHandle) -> KeyAction {
    // Ctrl-C always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return KeyAction::Quit;
    }

    // If a modal is open, handle modal keys first
    if app.modal.is_some() {
        return handle_modal_key(key, app, state_handle).await;
    }

    // If in review mode, handle review keys
    if app.review.is_some() {
        return handle_review_key(key, app, state_handle).await;
    }

    match key.code {
        // -- Global keys --
        KeyCode::Char('q') => {
            if app.sessions.iter().any(|s| {
                matches!(s.session.state, session::SessionState::Running | session::SessionState::Blocked { .. })
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
                buffer: String::new(),
            });
        }

        // -- Tab navigation --
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

        // -- Orc tab keys --
        KeyCode::Char('g') if app.focused_tab == TabId::Orc => {
            app.show_graph = !app.show_graph;
        }

        // -- Worker tab keys --
        KeyCode::Enter => {
            if let TabId::Worker(idx) = app.focused_tab {
                if let Some(sv) = app.sessions.get(idx) {
                    return KeyAction::Attach(sv.session.tmux_session.clone());
                }
            }
        }
        KeyCode::Char('c') => {
            if let TabId::Worker(idx) = app.focused_tab {
                if let Some(sv) = app.sessions.get(idx) {
                    let new_mode = match sv.session.mode {
                        session::SessionMode::Watch => session::SessionMode::Control,
                        session::SessionMode::Control => session::SessionMode::Watch,
                    };
                    let _ = state_handle.send(StateCommand::SetMode {
                        session_id: sv.session.id.clone(),
                        mode: new_mode,
                        reply: tokio::sync::oneshot::channel().0,
                    }).await;
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
            }
        }
        KeyCode::Char('R') => {
            if let TabId::Worker(idx) = app.focused_tab {
                if let Some(sv) = app.sessions.get(idx) {
                    if matches!(sv.session.state, session::SessionState::Failed { .. }) {
                        let name = sv.session.name.clone();
                        let task = sv.session.task.clone();
                        let worktree_path = sv.session.worktree_path.clone();
                        let branch = sv.session.branch.clone();
                        let base_commit = sv.session.base_commit.clone();
                        let model = sv.session.model.clone();
                        let old_tmux = sv.session.tmux_session.clone();
                        let old_id = sv.session.id.clone();

                        // Kill old tmux session
                        let _ = tmux::kill_session(&old_tmux).await;

                        // Remove old session from state
                        let _ = state_handle.send(StateCommand::RemoveSession {
                            session_id: old_id,
                        }).await;
                        app.remove_session(&sv.session.id.clone());

                        // Create new session with same params
                        match state_handle
                            .create_session(&name, &task, &worktree_path, &branch, &base_commit, &model)
                            .await
                        {
                            Ok(new_session) => {
                                let wt = std::path::Path::new(&worktree_path);
                                if let Err(e) = tmux::create_session(&new_session.tmux_session, wt).await {
                                    app.push_chat(ChatRole::System, format!("restart tmux error: {e}"));
                                } else {
                                    // Send claude command to new tmux session
                                    let claude_cmd = format!(
                                        "claude -p --model {} --permission-mode plan \"{}\"",
                                        model,
                                        task.replace('"', "\\\"")
                                    );
                                    let _ = tmux::send_text(&new_session.tmux_session, &claude_cmd).await;
                                    let _ = tmux::send_keys(&new_session.tmux_session, &["Enter"]).await;
                                    app.push_chat(
                                        ChatRole::System,
                                        format!("restarted session '{name}' (failed -> running)"),
                                    );
                                }
                            }
                            Err(e) => {
                                app.push_chat(ChatRole::System, format!("restart error: {e}"));
                            }
                        }
                    }
                }
            }
        }
        KeyCode::Char('r') => {
            if let TabId::Worker(idx) = app.focused_tab {
                if let Some(sv) = app.sessions.get(idx) {
                    if let session::SessionState::AwaitingReview { ref diff_hash } = sv.session.state {
                        // Compute diff and enter review mode
                        match review::compute_diff(&sv.session.worktree_path, &sv.session.base_commit).await {
                            Ok(diff) => {
                                app.review = Some(review::ReviewState::new(
                                    sv.session.id.clone(),
                                    diff,
                                ));
                            }
                            Err(e) => {
                                app.push_chat(ChatRole::System, format!("diff error: {e}"));
                            }
                        }
                    }
                }
            }
        }

        // -- Scroll --
        KeyCode::Down | KeyCode::Char('j') => {
            app.scroll_down(app.focused_tab, 1);
        }
        KeyCode::Up | KeyCode::Char('k') if app.modal.is_none() => {
            // 'k' for scroll only when not about to open kill modal
            // handled above for kill, so this only triggers if not on worker tab
            app.scroll_up(app.focused_tab, 1);
        }
        KeyCode::PageDown => app.scroll_down(app.focused_tab, 10),
        KeyCode::PageUp => app.scroll_up(app.focused_tab, 10),

        _ => {}
    }

    KeyAction::None
}

async fn handle_modal_key(key: KeyEvent, app: &mut App, state_handle: &StateHandle) -> KeyAction {
    let modal = app.modal.take(); // take ownership temporarily
    match modal {
        Some(Modal::NewTask { mut buffer }) => {
            match key.code {
                KeyCode::Esc => {} // modal dismissed (already taken)
                KeyCode::Enter if !buffer.is_empty() => {
                    let msg = buffer.clone();
                    return KeyAction::SendToOrc(msg);
                }
                KeyCode::Char(c) => {
                    buffer.push(c);
                    app.modal = Some(Modal::NewTask { buffer });
                }
                KeyCode::Backspace => {
                    buffer.pop();
                    app.modal = Some(Modal::NewTask { buffer });
                }
                _ => {
                    app.modal = Some(Modal::NewTask { buffer });
                }
            }
        }
        Some(Modal::AskUser {
            session_id,
            question,
            context,
            mut buffer,
        }) => {
            match key.code {
                KeyCode::Esc => {} // defer — dismiss modal
                KeyCode::Enter if !buffer.is_empty() => {
                    let _ = state_handle
                        .answer_user(&session_id, &buffer)
                        .await;
                }
                KeyCode::Char(c) => {
                    buffer.push(c);
                    app.modal = Some(Modal::AskUser {
                        session_id,
                        question,
                        context,
                        buffer,
                    });
                }
                KeyCode::Backspace => {
                    buffer.pop();
                    app.modal = Some(Modal::AskUser {
                        session_id,
                        question,
                        context,
                        buffer,
                    });
                }
                _ => {
                    app.modal = Some(Modal::AskUser {
                        session_id,
                        question,
                        context,
                        buffer,
                    });
                }
            }
        }
        Some(Modal::ConfirmKill { session_id, name }) => {
            match key.code {
                KeyCode::Char('y') => {
                    // Kill the session's tmux + remove from state
                    let _ = tmux::kill_session(&format!("orc-{name}")).await;
                    state_handle
                        .send(StateCommand::RemoveSession {
                            session_id: session_id.clone(),
                        })
                        .await
                        .ok();
                    app.remove_session(&session_id);
                }
                KeyCode::Esc | KeyCode::Char('n') => {}
                _ => {
                    app.modal = Some(Modal::ConfirmKill { session_id, name });
                }
            }
        }
        Some(Modal::ConfirmQuit) => {
            match key.code {
                KeyCode::Char('y') => return KeyAction::Quit,
                KeyCode::Esc | KeyCode::Char('n') => {}
                _ => {
                    app.modal = Some(Modal::ConfirmQuit);
                }
            }
        }
        Some(Modal::Help) => {
            // Any key dismisses help
        }
        None => {}
    }
    KeyAction::None
}

// ---------------------------------------------------------------------------
// State change handling
// ---------------------------------------------------------------------------

async fn handle_review_key(key: KeyEvent, app: &mut App, state_handle: &StateHandle) -> KeyAction {
    let review = match app.review.as_mut() {
        Some(r) => r,
        None => return KeyAction::None,
    };

    match key.code {
        // Navigation
        KeyCode::Char('j') | KeyCode::Down => review.move_line_down(),
        KeyCode::Char('k') | KeyCode::Up => review.move_line_up(),
        KeyCode::Char('J') => review.move_hunk_down(),
        KeyCode::Char('K') => review.move_hunk_up(),

        // Approve hunk
        KeyCode::Char('a') => {
            review.toggle_hunk_approval();
        }

        // Comment — enter a simple inline comment
        KeyCode::Char('c') => {
            // For now, use a modal for comment input
            if let Some(file) = review.current_file() {
                let line = review.current_line()
                    .and_then(|l| l.new_lineno)
                    .unwrap_or(0);
                app.modal = Some(Modal::NewTask {
                    buffer: String::new(), // reuse NewTask modal for comment input
                });
                // TODO: dedicated comment modal in Phase 5
            }
        }

        // Submit review
        KeyCode::Char('s') => {
            let payload = review.to_payload();
            let session_id = review.session_id.clone();
            let payload_json = serde_json::to_string_pretty(&payload).unwrap_or_default();

            // Send review payload to the worker session via tmux
            if let Some(sv) = app.sessions.iter().find(|s| s.session.id == session_id) {
                let tmux_name = &sv.session.tmux_session;
                // Send the review as the next prompt to the worker
                let _ = tmux::send_text(tmux_name, &payload_json).await;
                let _ = tmux::send_keys(tmux_name, &["Enter"]).await;
                app.push_chat(ChatRole::System, format!("review submitted for {}", sv.session.name));
            }

            // Apply ReviewSubmitted event
            let _ = state_handle.send(StateCommand::ApplyEvent {
                session_id: session_id.clone(),
                event: session::SessionEvent::ReviewSubmitted {
                    approved: true,
                    feedback: Some(payload_json),
                },
                reply: tokio::sync::oneshot::channel().0,
            }).await;

            app.review = None;
        }

        // Cancel review
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
            app.push_chat(
                ChatRole::System,
                format!("session {}: {}", &session_id[..8], label),
            );
        }
        StateChange::SessionModeChanged { session_id, mode } => {
            app.update_session_mode(&session_id, mode);
        }
        StateChange::SessionRemoved { session_id } => {
            app.remove_session(&session_id);
        }
        StateChange::UserQuestionPending {
            session_id,
            question,
            context,
        } => {
            app.modal = Some(Modal::AskUser {
                session_id,
                question,
                context,
                buffer: String::new(),
            });
        }
        StateChange::PermissionNeeded {
            session_id,
            request,
        } => {
            app.push_chat(
                ChatRole::System,
                format!(
                    "permission escalated: {} for {}",
                    request.tool_name, &session_id[..8]
                ),
            );
        }
        StateChange::TaskGraphUpdated { .. } => {}
        StateChange::HookReceived { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// Orc brain event handling
// ---------------------------------------------------------------------------

fn handle_orc_event(app: &mut App, event: OrcEvent) {
    match event {
        OrcEvent::Text(text) => {
            app.push_chat(ChatRole::Orc, text);
        }
        OrcEvent::ToolUse { name, input, .. } => {
            app.push_chat(
                ChatRole::System,
                format!("orc calling: {name}({})", truncate_json(&input)),
            );
        }
        OrcEvent::Result {
            is_error,
            result,
            cost_usd,
            ..
        } => {
            if is_error {
                app.push_chat(ChatRole::System, format!("orc error: {result}"));
            } else if let Some(cost) = cost_usd {
                app.push_chat(
                    ChatRole::System,
                    format!("turn complete (${:.4})", cost),
                );
            }
        }
        OrcEvent::System { model, .. } => {
            if let Some(m) = model {
                app.push_chat(ChatRole::System, format!("orc model: {m}"));
            }
        }
        OrcEvent::Thinking(_) => {
            // Don't show thinking in chat — too verbose
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

// ---------------------------------------------------------------------------
// tmux attach/detach flow
// ---------------------------------------------------------------------------

async fn do_attach(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    session_name: &str,
) -> Result<()> {
    // Leave alt-screen so tmux attach gets the raw terminal
    terminal::disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    // Blocking attach — returns when user detaches (C-q)
    let result = tmux::attach(session_name).await;

    // Resume alt-screen
    io::stdout().execute(EnterAlternateScreen)?;
    terminal::enable_raw_mode()?;
    terminal.clear()?;

    if let Err(e) = result {
        eprintln!("[orc] attach error: {e}");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Doctor subcommand
// ---------------------------------------------------------------------------

async fn run_doctor() -> Result<()> {
    println!("orc doctor — checking environment\n");
    let mut all_ok = true;

    // Check git
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

    // Check claude CLI
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

    // Check tmux
    match std::process::Command::new("tmux").arg("-V").output() {
        Ok(o) if o.status.success() => {
            let ver = String::from_utf8_lossy(&o.stdout);
            println!("\u{2713} tmux: {}", ver.trim());
        }
        _ => {
            println!("\u{2717} tmux: not found or not functional");
            all_ok = false;
        }
    }

    // Check orphaned tmux sessions
    match tmux::list_orc_sessions().await {
        Ok(sessions) if sessions.is_empty() => {
            println!("\u{2713} no orphaned orc-* tmux sessions");
        }
        Ok(sessions) => {
            println!("\u{2717} found {} orphaned orc-* tmux sessions:", sessions.len());
            for s in &sessions {
                println!("    - {s}");
            }
            all_ok = false;
        }
        Err(_) => {
            println!("\u{2713} no orphaned orc-* tmux sessions (no tmux server)");
        }
    }

    // Check data directory
    let data_dir = dirs_data_dir().join("orc");
    if data_dir.exists() {
        // Test writability
        let test_file = data_dir.join(".doctor-probe");
        match std::fs::write(&test_file, "probe") {
            Ok(_) => {
                let _ = std::fs::remove_file(&test_file);
                println!("\u{2713} data directory: {} (writable)", data_dir.display());
            }
            Err(e) => {
                println!("\u{2717} data directory: {} (not writable: {e})", data_dir.display());
                all_ok = false;
            }
        }
    } else {
        println!("\u{2717} data directory: {} (does not exist)", data_dir.display());
        all_ok = false;
    }

    // Check SQLite database
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn dirs_data_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".config")
    } else {
        PathBuf::from(".")
    }
}
