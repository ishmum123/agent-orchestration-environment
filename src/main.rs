use anyhow::{bail, Result};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers, EnableBracketedPaste, DisableBracketedPaste},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::env;
use std::io;
use std::process::Command;
use std::time::Duration;

mod agent;
mod app;
mod claude;
mod events;
mod orc;
mod ui;
mod worktree;

use agent::OutputEntry;
use app::{App, AppMode, ConfirmCallback};

#[derive(Parser)]
#[command(
    name = "orc",
    about = "Parallel Claude Code agents with intelligent orchestration"
)]
struct Cli {
    /// Project directory (defaults to current directory)
    #[arg(short, long, default_value = ".")]
    project: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    check_dependencies()?;

    let mut app = App::new(&cli.project);

    // Spawn orc brain
    match orc::spawn_orc(&cli.project) {
        Ok(proc) => {
            app.orc = Some(proc);
            app.orc_output.push(OutputEntry::Text(
                "ready. type below to chat, or press **n** to spawn an agent directly.".to_string()
            ));
            app.set_status("orc started".to_string());
        }
        Err(e) => {
            bail!("failed to start orc: {}", e);
        }
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableBracketedPaste)?;
    terminal.show_cursor()?;

    app.cleanup();

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        // Drain events from all processes (non-blocking)
        let completions = app.drain_events();

        // Notify orc about agent completions
        for (name, result, is_error) in &completions {
            if let Some(ref mut orc) = app.orc {
                let status = if *is_error { "failed" } else { "finished" };
                let msg = if result.is_empty() {
                    format!("Agent \"{}\" {}.", name, status)
                } else {
                    let truncated = if result.len() > 2000 { &result[..2000] } else { result.as_str() };
                    format!("Agent \"{}\" {}. Result:\n{}", name, status, truncated)
                };
                orc.send(&msg).ok();
            }
        }

        app.process_orc_commands()?;
        app.prune_dead_agents()?;

        // Check agent context sizes and alert orc
        let context_alerts = app.check_agent_context();
        for alert in &context_alerts {
            if let Some(ref mut orc) = app.orc {
                orc.send(alert).ok();
            }
        }

        app.tick = app.tick.wrapping_add(1);

        terminal.draw(|f| ui::render(f, app))?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                        app.should_quit = true;
                    } else {
                        match &app.mode {
                            AppMode::OrcDashboard => handle_dashboard_key(app, key.code, key.modifiers)?,
                            AppMode::AgentPanel => handle_agent_panel_key(app, key.code)?,
                            AppMode::OrcInput | AppMode::SpawnInput
                                | AppMode::TellInput { .. } | AppMode::DirectSendInput { .. } => {
                                handle_input_key(app, key.code)?
                            }
                            AppMode::AgentDashboard { .. } => handle_agent_browse_key(app, key.code, key.modifiers)?,
                            AppMode::AgentInput { .. } => handle_agent_input_key(app, key.code, key.modifiers)?,
                            AppMode::Help => {
                                app.mode = AppMode::OrcDashboard;
                            }
                            AppMode::Confirm { .. } => {
                                handle_confirm_key(app, key.code)?;
                            }
                        }
                    }
                }
                Event::Paste(text) => {
                    let cleaned = text.replace('\r', "");
                    if matches!(app.mode, AppMode::AgentInput { .. }) {
                        app.agent_input_buf.push_str(&cleaned);
                    } else {
                        if !app.mode.is_text_input() {
                            app.mode = AppMode::OrcInput;
                            app.input_buf.clear();
                        }
                        app.input_buf.push_str(&cleaned);
                    }
                }
                _ => {}
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

// --- Key handlers ---

fn handle_dashboard_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Result<()> {
    match code {
        KeyCode::Esc => enter_chat_mode(app),
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => {
            app.scroll_offset = app.scroll_offset.saturating_sub(1);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.scroll_offset = app.scroll_offset.saturating_add(1);
        }
        KeyCode::Enter => enter_chat_mode(app),
        KeyCode::Char('a') => {
            if !app.agents.is_empty() {
                app.mode = AppMode::AgentPanel;
            }
        }
        KeyCode::Char('n') => {
            app.mode = AppMode::SpawnInput;
            app.input_buf.clear();
        }
        KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_offset = app.scroll_offset.saturating_sub(5);
        }
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_offset = app.scroll_offset.saturating_add(5);
        }
        KeyCode::Char('e') => open_editor_subprocess(app)?,
        KeyCode::Char('?') => app.mode = AppMode::Help,
        KeyCode::Tab => app.show_preview = !app.show_preview,
        _ => {}
    }
    Ok(())
}

fn handle_agent_panel_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Esc => {
            app.mode = AppMode::OrcDashboard;
        }
        KeyCode::Char('j') | KeyCode::Down => app.select_next(),
        KeyCode::Char('k') | KeyCode::Up => app.select_prev(),
        KeyCode::Enter => {
            if app.selected_agent().is_some() {
                enter_agent_mode(app, app.selected);
            }
        }
        KeyCode::Char('t') => {
            if app.selected_agent().is_some() {
                let name = app.agents[app.selected].name.clone();
                app.mode = AppMode::TellInput { agent_name: name };
                app.input_buf.clear();
            }
        }
        KeyCode::Char('/') => {
            if app.selected_agent().is_some() {
                let name = app.agents[app.selected].name.clone();
                app.mode = AppMode::DirectSendInput { agent_name: name };
                app.input_buf.clear();
            }
        }
        KeyCode::Char('x') => {
            if app.selected_agent().is_some() {
                let name = app.agents[app.selected].name.clone();
                app.mode = AppMode::Confirm {
                    message: format!("kill {}?", name),
                    callback: ConfirmCallback::KillAgent(app.selected),
                };
            }
        }
        _ => {}
    }
    Ok(())
}

fn enter_chat_mode(app: &mut App) {
    app.mode = AppMode::OrcInput;
    app.input_buf.clear();
}

fn enter_agent_mode(app: &mut App, idx: usize) {
    if let Some(agent) = app.agents.get(idx) {
        let name = agent.name.clone();
        app.mode = AppMode::AgentInput {
            agent_idx: idx,
            scroll: 0,
        };
        app.agent_input_buf.clear();
        app.agent_input_name = name;
    }
}

fn handle_input_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Enter => {
            let input = app.input_buf.clone();
            if input.is_empty() {
                return Ok(());
            }
            let mode = std::mem::replace(&mut app.mode, AppMode::OrcDashboard);
            match mode {
                AppMode::SpawnInput => app.spawn_agent(&input)?,
                AppMode::TellInput { .. } => app.tell_agent(&input)?,
                AppMode::DirectSendInput { .. } => app.direct_send(&input)?,
                AppMode::OrcInput => app.chat_orc(&input)?,
                _ => {}
            }
            enter_chat_mode(app);
            Ok(())
        }
        KeyCode::Char(c) => {
            app.input_buf.push(c);
            Ok(())
        }
        KeyCode::Backspace => {
            app.input_buf.pop();
            Ok(())
        }
        KeyCode::Esc => {
            app.mode = AppMode::OrcDashboard;
            Ok(())
        }
        _ => Ok(()),
    }
}

fn handle_agent_browse_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
    match code {
        KeyCode::Esc => {
            app.mode = AppMode::AgentPanel;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if let AppMode::AgentDashboard { scroll, .. } = &mut app.mode {
                *scroll = scroll.saturating_sub(1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if let AppMode::AgentDashboard { scroll, .. } = &mut app.mode {
                *scroll = scroll.saturating_add(1);
            }
        }
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
            if let AppMode::AgentDashboard { scroll, .. } = &mut app.mode {
                *scroll = scroll.saturating_add(10);
            }
        }
        KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
            if let AppMode::AgentDashboard { scroll, .. } = &mut app.mode {
                *scroll = scroll.saturating_sub(10);
            }
        }
        KeyCode::Char('i') | KeyCode::Enter => {
            if let AppMode::AgentDashboard { agent_idx, scroll } = app.mode {
                app.mode = AppMode::AgentInput { agent_idx, scroll };
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_agent_input_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
    match code {
        KeyCode::Esc => {
            app.agent_input_buf.clear();
            if let AppMode::AgentInput { agent_idx, scroll } = app.mode {
                app.mode = AppMode::AgentDashboard { agent_idx, scroll };
            }
        }
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
            if let AppMode::AgentInput { scroll, .. } = &mut app.mode {
                *scroll = scroll.saturating_add(5);
            }
        }
        KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
            if let AppMode::AgentInput { scroll, .. } = &mut app.mode {
                *scroll = scroll.saturating_sub(5);
            }
        }
        KeyCode::Enter => {
            if !app.agent_input_buf.is_empty() {
                let input = app.agent_input_buf.clone();
                if let AppMode::AgentInput { agent_idx, .. } = &app.mode {
                    let idx = *agent_idx;
                    let agent_name = app.agents.get(idx).map(|a| a.name.clone());
                    if let Some(agent) = app.agents.get_mut(idx) {
                        if let Some(ref mut proc) = agent.process {
                            proc.send(&input).ok();
                        }
                        agent.output.push_user_input(&input);
                        agent.prune_after_orc_prompt = None;
                    }
                    if let Some(name) = agent_name {
                        app.set_status(format!("sent to {}", name));
                    }
                }
                app.agent_input_buf.clear();
            }
        }
        KeyCode::Char(c) => {
            app.agent_input_buf.push(c);
        }
        KeyCode::Backspace => {
            app.agent_input_buf.pop();
        }
        _ => {}
    }
    Ok(())
}

fn handle_confirm_key(app: &mut App, code: KeyCode) -> Result<()> {
    let mode = std::mem::replace(&mut app.mode, AppMode::OrcDashboard);
    if let AppMode::Confirm { callback, .. } = mode {
        match code {
            KeyCode::Char('y') | KeyCode::Enter => match callback {
                ConfirmCallback::KillAgent(idx) => app.kill_agent(idx)?,
            },
            _ => {}
        }
    }
    Ok(())
}

fn open_editor_subprocess(app: &mut App) -> Result<()> {
    let agent = match app.selected_agent() {
        Some(a) => a,
        None => return Ok(()),
    };

    let worktree = match agent.worktree.to_str() {
        Some(s) => s.to_string(),
        None => return Ok(()),
    };

    let output = Command::new("git")
        .args(["diff", "--name-only", "HEAD"])
        .current_dir(&worktree)
        .output();

    let changed_files = match output {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
        _ => String::new(),
    };

    if changed_files.is_empty() {
        app.set_status("no changes to edit".to_string());
        return Ok(());
    }

    let editor = env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());
    let files: Vec<&str> = changed_files.lines().collect();
    let file_args: Vec<String> = files.iter().map(|f| format!("{}/{}", worktree, f)).collect();

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, DisableBracketedPaste)?;

    Command::new(&editor)
        .args(&file_args)
        .status()
        .ok();

    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;

    Ok(())
}

fn check_dependencies() -> Result<()> {
    let required = [
        ("git", "git is required for worktree isolation"),
        ("claude", "Claude Code CLI is required"),
    ];

    let mut missing = Vec::new();
    for (cmd, reason) in &required {
        if Command::new("sh").args(["-c", &format!("command -v {}", cmd)]).output().map(|o| !o.status.success()).unwrap_or(true) {
            missing.push(format!("  {} \u{2014} {}", cmd, reason));
        }
    }

    if !missing.is_empty() {
        bail!("missing required dependencies:\n{}", missing.join("\n"));
    }

    Ok(())
}
