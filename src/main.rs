use anyhow::{bail, Result};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers, EnableBracketedPaste, DisableBracketedPaste},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::env;
use std::fs;
use std::io;
use std::process::Command;
use std::time::Duration;

mod agent;
mod app;
mod events;
mod orc;
mod tmux;
mod ui;
mod worktree;

use app::{App, AppMode, ConfirmCallback, InputCallback};

#[derive(Parser)]
#[command(
    name = "orc",
    about = "Parallel Claude Code agents with intelligent orchestration"
)]
struct Cli {
    /// Project directory (defaults to current directory)
    #[arg(short, long, default_value = ".")]
    project: String,

    /// tmux session name
    #[arg(short, long, default_value = "orc")]
    session: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    check_dependencies()?;

    let mut app = App::new(&cli.session, &cli.project);

    // Kill existing session if any
    if app.session.exists() {
        app.session.kill()?;
    }

    // Generate lazygit config
    ensure_lazygit_config()?;

    // Spawn orc — creates tmux session with claude directly in pane 0
    app.orc_pane = Some(orc::spawn_orc(&app.session, &cli.project)?);
    app.set_status("orc started".to_string());

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

    // Cleanup
    app.session.kill().ok();
    for agent in &app.agents {
        worktree::remove_worktree(&app.project_dir, &agent.name).ok();
    }
    let orc_dir = dirs::home_dir().unwrap_or_default().join(".orc");
    fs::remove_file(orc_dir.join("locked")).ok();
    fs::remove_dir_all(orc_dir.join("orc")).ok();
    fs::remove_dir_all(orc_dir.join("agents")).ok();

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    let poll_interval = Duration::from_secs(2);

    loop {
        terminal.draw(|f| ui::render(f, app))?;

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) => {
                    match &app.mode {
                        AppMode::Dashboard => {
                            handle_dashboard_key(app, key.code, key.modifiers)?
                        }
                        AppMode::Input { .. } => handle_input_key(app, key.code)?,
                        AppMode::Status => handle_status_key(app, key.code)?,
                        AppMode::AgentDetail { .. } => handle_detail_key(app, key.code, key.modifiers)?,
                        AppMode::Help => {
                            app.mode = AppMode::Dashboard;
                        }
                        AppMode::Confirm { .. } => {
                            handle_confirm_key(app, key.code)?;
                        }
                    }
                }
                Event::Paste(text) => {
                    let cleaned = text.replace('\r', "");
                    if matches!(app.mode, AppMode::AgentDetail { .. }) {
                        app.agent_input_buf.push_str(&cleaned);
                    } else {
                        if !matches!(app.mode, AppMode::Input { .. }) {
                            app.mode = AppMode::Input {
                                prompt_label: "> ".to_string(),
                                callback: InputCallback::ChatOrc,
                            };
                            app.input_buf.clear();
                        }
                        app.input_buf.push_str(&cleaned);
                    }
                }
                _ => {}
            }
        }

        // Periodic polling (orc + agents)
        if app.last_poll.elapsed() > poll_interval {
            app.poll_agents().ok();
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn handle_dashboard_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Result<()> {
    match code {
        KeyCode::Esc => enter_chat_mode(app),
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => app.select_next(),
        KeyCode::Char('k') | KeyCode::Up => app.select_prev(),
        KeyCode::Enter => {
            if app.selected_agent().is_some() {
                enter_agent_mode(app, app.selected);
            } else {
                enter_chat_mode(app);
            }
        }
        KeyCode::Char('n') => {
            app.mode = AppMode::Input {
                prompt_label: "task> ".to_string(),
                callback: InputCallback::NewAgent,
            };
            app.input_buf.clear();
        }
        KeyCode::Char('t') => {
            if app.selected_agent().is_some() {
                let name = app.agents[app.selected].name.clone();
                app.mode = AppMode::Input {
                    prompt_label: format!("tell {}> ", name),
                    callback: InputCallback::TellAgent,
                };
                app.input_buf.clear();
            }
        }
        KeyCode::Char('/') => {
            if app.selected_agent().is_some() {
                let name = app.agents[app.selected].name.clone();
                app.mode = AppMode::Input {
                    prompt_label: format!("{}> ", name),
                    callback: InputCallback::DirectSend,
                };
                app.input_buf.clear();
            }
        }
        KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_offset = app.scroll_offset.saturating_sub(5);
        }
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_offset = app.scroll_offset.saturating_add(5);
        }
        KeyCode::Char('e') => open_editor_subprocess(app)?,
        KeyCode::Char('s') => {
            app.status_selected = 0;
            app.mode = AppMode::Status;
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
        KeyCode::Char('?') => app.mode = AppMode::Help,
        KeyCode::Tab => app.show_preview = !app.show_preview,
        _ => {}
    }
    Ok(())
}

fn enter_chat_mode(app: &mut App) {
    app.mode = AppMode::Input {
        prompt_label: "> ".to_string(),
        callback: InputCallback::ChatOrc,
    };
    app.input_buf.clear();
}

fn enter_agent_mode(app: &mut App, idx: usize) {
    if let Some(agent) = app.agents.get(idx) {
        let name = agent.name.clone();
        app.mode = AppMode::AgentDetail {
            agent_idx: idx,
            scroll: 0,
        };
        app.agent_input_buf.clear();
        app.agent_input_name = name;
    }
}

fn handle_input_key(
    app: &mut App,
    code: KeyCode,
) -> Result<()> {
    match code {
        KeyCode::Enter => {
            let input = app.input_buf.clone();
            if input.is_empty() {
                return Ok(());
            }
            let mode = std::mem::replace(&mut app.mode, AppMode::Dashboard);
            if let AppMode::Input { callback, .. } = mode {
                match callback {
                    InputCallback::NewAgent => spawn_new_agent(app, &input)?,
                    InputCallback::TellAgent => tell_agent(app, &input)?,
                    InputCallback::DirectSend => direct_send(app, &input)?,
                    InputCallback::ChatOrc => chat_orc(app, &input)?,
                }
            }
            // Return to chat mode after sending
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
            app.mode = AppMode::Dashboard;
            Ok(())
        }
        _ => Ok(()),
    }
}

fn handle_status_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('s') => {
            app.mode = AppMode::Dashboard;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if !app.agents.is_empty() {
                app.status_selected = (app.status_selected + 1) % app.agents.len();
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if !app.agents.is_empty() {
                app.status_selected = if app.status_selected == 0 {
                    app.agents.len() - 1
                } else {
                    app.status_selected - 1
                };
            }
        }
        KeyCode::Enter => {
            if !app.agents.is_empty() {
                app.mode = AppMode::AgentDetail {
                    agent_idx: app.status_selected,
                    scroll: 0,
                };
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_detail_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
    match code {
        KeyCode::Esc => {
            if app.agent_input_buf.is_empty() {
                enter_chat_mode(app);
            } else {
                app.agent_input_buf.clear();
            }
        }
        // Scroll with Ctrl+U / Ctrl+D
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
            if let AppMode::AgentDetail { scroll, .. } = &mut app.mode {
                *scroll = scroll.saturating_add(5);
            }
        }
        KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
            if let AppMode::AgentDetail { scroll, .. } = &mut app.mode {
                *scroll = scroll.saturating_sub(5);
            }
        }
        KeyCode::Enter => {
            if !app.agent_input_buf.is_empty() {
                let input = app.agent_input_buf.clone();
                if let AppMode::AgentDetail { agent_idx, .. } = &app.mode {
                    let idx = *agent_idx;
                    if let Some(agent) = app.agents.get(idx) {
                        let flat = input.lines()
                            .map(|l| l.trim())
                            .filter(|l| !l.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ");
                        let tmp_path = dirs::home_dir()
                            .unwrap_or_default()
                            .join(".orc/msg.tmp");
                        fs::write(&tmp_path, &flat)?;
                        Command::new("tmux")
                            .args(["load-buffer", tmp_path.to_str().unwrap()])
                            .status().ok();
                        Command::new("tmux")
                            .args(["paste-buffer", "-t", &agent.pane.target()])
                            .status().ok();
                        tmux::send_keys_raw(&agent.pane, "Enter")?;
                        fs::remove_file(&tmp_path).ok();
                        app.set_status(format!("sent to {}", agent.name));
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
    let mode = std::mem::replace(&mut app.mode, AppMode::Dashboard);
    if let AppMode::Confirm { callback, .. } = mode {
        match code {
            KeyCode::Char('y') | KeyCode::Enter => match callback {
                ConfirmCallback::KillAgent(idx) => kill_agent(app, idx)?,
            },
            _ => {}
        }
    }
    Ok(())
}

fn chat_orc(app: &mut App, message: &str) -> Result<()> {
    if let Some(orc_pane) = &app.orc_pane {
        // For multi-line or long messages, use tmux load-buffer to avoid
        // send-keys issues with special characters and newlines
        let tmp_path = dirs::home_dir()
            .unwrap_or_default()
            .join(".orc/msg.tmp");
        // Collapse newlines to spaces for a single-message send
        let flat = message.lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        fs::write(&tmp_path, &flat)?;

        // Load into tmux buffer and paste into the pane
        Command::new("tmux")
            .args(["load-buffer", tmp_path.to_str().unwrap()])
            .status()
            .ok();
        Command::new("tmux")
            .args(["paste-buffer", "-t", &orc_pane.target()])
            .status()
            .ok();
        // Send Enter to submit
        tmux::send_keys_raw(orc_pane, "Enter")?;
        fs::remove_file(&tmp_path).ok();

        app.set_status("sent to orc".to_string());
    }
    Ok(())
}

fn spawn_new_agent(app: &mut App, input: &str) -> Result<()> {
    // Parse "name: description" or auto-generate slug from description
    let (name, description) = if let Some((n, d)) = input.split_once(':') {
        let n = n.trim();
        let d = d.trim();
        if !n.is_empty() && !d.is_empty() {
            (n.to_string(), d.to_string())
        } else {
            let slug = agent::slugify_description(input);
            (dedupe_name(&slug, &app.agents), input.to_string())
        }
    } else {
        let slug = agent::slugify_description(input);
        (dedupe_name(&slug, &app.agents), input.to_string())
    };

    // Create worktree
    let worktree_path = worktree::create_worktree(&app.project_dir, &name)?;

    // Create tmux pane (background — user never sees it)
    let pane = app
        .session
        .create_pane(worktree_path.to_str().unwrap())?;

    // Launch claude in the pane
    tmux::send_keys(&pane, "claude")?;

    // Wait for claude to start, then send the prompt
    std::thread::sleep(Duration::from_secs(3));
    tmux::send_keys(&pane, &description)?;

    let pane_id = pane.pane;
    let agent = agent::Agent::new(name.clone(), description.clone(), pane, worktree_path);
    app.agents.push(agent);

    // Notify orc about the new agent
    if let Some(orc_pane) = &app.orc_pane {
        orc::notify_agent_spawned(orc_pane, &name, pane_id, &description)?;
    }

    app.set_status(format!("spawned '{}'", name));
    Ok(())
}

fn tell_agent(app: &mut App, message: &str) -> Result<()> {
    if let Some(orc_pane) = &app.orc_pane {
        let agent_name = &app.agents[app.selected].name;
        let relay_msg = format!(
            "The user wants to tell agent \"{}\": \"{}\". Enrich this with relevant context and send it to the agent's pane using tmux send-keys.",
            agent_name, message
        );
        tmux::send_keys(orc_pane, &relay_msg)?;
        app.set_status(format!("told {} (via orc)", agent_name));
    }
    Ok(())
}

fn direct_send(app: &mut App, message: &str) -> Result<()> {
    let agent = &app.agents[app.selected];
    tmux::send_keys(&agent.pane, message)?;
    app.set_status(format!("sent to {}", agent.name));
    Ok(())
}

fn kill_agent(app: &mut App, idx: usize) -> Result<()> {
    if idx >= app.agents.len() {
        return Ok(());
    }

    let agent = &app.agents[idx];
    let name = agent.name.clone();

    // Kill the tmux pane
    app.session.kill_pane(&agent.pane).ok();

    // Remove worktree
    worktree::remove_worktree(&app.project_dir, &name).ok();

    // Remove orc metadata file if it exists
    let meta_path = dirs::home_dir()
        .unwrap_or_default()
        .join(format!(".orc/agents/{}.json", name));
    fs::remove_file(meta_path).ok();

    app.agents.remove(idx);

    // Fix selection index
    if app.selected >= app.agents.len() && !app.agents.is_empty() {
        app.selected = app.agents.len() - 1;
    }

    app.set_status(format!("killed '{}'", name));
    Ok(())
}

fn open_editor_subprocess(app: &mut App) -> Result<()> {
    let agent = match app.selected_agent() {
        Some(a) => a,
        None => return Ok(()),
    };

    let worktree = agent.worktree.to_str().unwrap().to_string();

    // Get changed files
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

    // Run editor as a child process (not via tmux)
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;

    Command::new(&editor)
        .args(&file_args)
        .status()
        .ok();

    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;

    Ok(())
}

/// Ensure agent name is unique by appending -2, -3, etc.
fn dedupe_name(base: &str, agents: &[agent::Agent]) -> String {
    let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
    if !names.contains(&base) {
        return base.to_string();
    }
    for i in 2..100 {
        let candidate = format!("{}-{}", base, i);
        if !names.contains(&candidate.as_str()) {
            return candidate;
        }
    }
    base.to_string()
}

fn check_dependencies() -> Result<()> {
    let required = [
        ("tmux", "tmux is required for session management"),
        ("git", "git is required for worktree isolation"),
        ("claude", "Claude Code CLI is required for the orchestrator and agents"),
    ];
    let optional = [
        ("lazygit", "[d]iff view requires lazygit"),
        ("delta", "lazygit pager uses delta for diffs"),
    ];

    let mut missing = Vec::new();
    for (cmd, reason) in &required {
        if Command::new("which").arg(cmd).output().map(|o| !o.status.success()).unwrap_or(true) {
            missing.push(format!("  {} \u{2014} {}", cmd, reason));
        }
    }

    if !missing.is_empty() {
        bail!("missing required dependencies:\n{}", missing.join("\n"));
    }

    for (cmd, reason) in &optional {
        if Command::new("which").arg(cmd).output().map(|o| !o.status.success()).unwrap_or(true) {
            eprintln!("warning: {} not found \u{2014} {}", cmd, reason);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_agent(name: &str) -> agent::Agent {
        agent::Agent::new(
            name.to_string(),
            "task".to_string(),
            tmux::TmuxPane { session: "s".to_string(), window: 0, pane: 0 },
            std::path::PathBuf::from("/tmp"),
        )
    }

    #[test]
    fn test_dedupe_name_no_conflict() {
        let agents = vec![make_agent("foo")];
        assert_eq!(dedupe_name("bar", &agents), "bar");
    }

    #[test]
    fn test_dedupe_name_conflict() {
        let agents = vec![make_agent("foo")];
        assert_eq!(dedupe_name("foo", &agents), "foo-2");
    }

    #[test]
    fn test_dedupe_name_multiple_conflicts() {
        let agents = vec![make_agent("foo"), make_agent("foo-2"), make_agent("foo-3")];
        assert_eq!(dedupe_name("foo", &agents), "foo-4");
    }

    #[test]
    fn test_dedupe_name_empty_agents() {
        let agents: Vec<agent::Agent> = vec![];
        assert_eq!(dedupe_name("test", &agents), "test");
    }
}

fn ensure_lazygit_config() -> Result<()> {
    let config_dir = dirs::home_dir()
        .unwrap_or_default()
        .join(".orc");
    fs::create_dir_all(&config_dir)?;

    let config_path = config_dir.join("lazygit.yml");
    if !config_path.exists() {
        let config = r#"gui:
  showBottomLine: false
  showCommandLog: false
  theme:
    activeBorderColor: ["green", "bold"]
git:
  paging:
    colorArg: always
    pager: delta --dark --paging=never
"#;
        fs::write(&config_path, config)?;
    }
    Ok(())
}
