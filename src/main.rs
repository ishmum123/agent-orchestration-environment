use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::fs;
use std::io;
use std::time::Duration;

mod agent;
mod app;
mod orc;
mod tmux;
mod ui;
mod worktree;

use app::{App, AppMode, InputCallback};

#[derive(Parser)]
#[command(
    name = "orchestr8",
    about = "Parallel Claude Code agents with intelligent orchestration"
)]
struct Cli {
    /// Project directory (defaults to current directory)
    #[arg(short, long, default_value = ".")]
    project: String,

    /// tmux session name
    #[arg(short, long, default_value = "orchestr8")]
    session: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut app = App::new(&cli.session, &cli.project);

    // Create tmux session
    if app.session.exists() {
        app.session.kill()?;
    }
    app.session.create()?;

    // Generate lazygit config
    ensure_lazygit_config()?;

    // Spawn the orchestrator
    app.orc_pane = Some(orc::spawn_orc(&app.session, &cli.project)?);
    app.status_line = "orc started".to_string();

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // Cleanup
    app.session.kill().ok();
    for agent in &app.agents {
        worktree::remove_worktree(&app.project_dir, &agent.name).ok();
    }
    let orchestr8_dir = dirs::home_dir().unwrap_or_default().join(".orchestr8");
    fs::remove_file(orchestr8_dir.join("locked")).ok();
    fs::remove_dir_all(orchestr8_dir.join("orc")).ok();

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
            if let Event::Key(key) = event::read()? {
                match &app.mode {
                    AppMode::Dashboard => {
                        handle_dashboard_key(app, terminal, key.code, key.modifiers)?
                    }
                    AppMode::Input { .. } => handle_input_key(app, terminal, key.code)?,
                    AppMode::Status => {
                        if matches!(
                            key.code,
                            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('s')
                        ) {
                            app.mode = AppMode::Dashboard;
                        }
                    }
                    AppMode::Attached => {} // shouldn't receive keys while attached
                }
            }
        }

        // Periodic agent polling
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
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Result<()> {
    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => app.select_next(),
        KeyCode::Char('k') | KeyCode::Up => app.select_prev(),
        KeyCode::Char('a') | KeyCode::Enter => attach_to_agent(app, terminal)?,
        KeyCode::Char('n') => {
            app.mode = AppMode::Input {
                prompt_label: "new agent> ".to_string(),
                callback: InputCallback::NewAgent,
            };
            app.input_buf.clear();
        }
        KeyCode::Char('t') => {
            if app.selected_agent().is_some() {
                app.mode = AppMode::Input {
                    prompt_label: format!("tell {}> ", app.agents[app.selected].name),
                    callback: InputCallback::TellAgent,
                };
                app.input_buf.clear();
            }
        }
        KeyCode::Char('d') => open_diff(app, terminal)?,
        KeyCode::Char('s') => app.mode = AppMode::Status,
        _ => {}
    }
    Ok(())
}

fn handle_input_key(
    app: &mut App,
    _terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    code: KeyCode,
) -> Result<()> {
    match code {
        KeyCode::Enter => {
            let input = app.input_buf.clone();
            if input.is_empty() {
                app.mode = AppMode::Dashboard;
                return Ok(());
            }
            let mode = std::mem::replace(&mut app.mode, AppMode::Dashboard);
            if let AppMode::Input { callback, .. } = mode {
                match callback {
                    InputCallback::NewAgent => spawn_new_agent(app, &input)?,
                    InputCallback::TellAgent => tell_agent(app, &input)?,
                }
            }
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

fn attach_to_agent(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    if let Some(agent) = app.agents.get_mut(app.selected) {
        agent.locked = true;
        let pane = agent.pane.clone();

        // Write lock file so orc knows not to send to this pane
        let lock_path = dirs::home_dir()
            .unwrap_or_default()
            .join(".orchestr8/locked");
        fs::create_dir_all(lock_path.parent().unwrap())?;
        fs::write(&lock_path, pane.pane.to_string())?;

        // Leave TUI temporarily
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

        // Select the agent's pane and attach
        app.session.select_pane(&pane)?;
        app.session.attach()?;

        // User detached — resume TUI
        enable_raw_mode()?;
        execute!(terminal.backend_mut(), EnterAlternateScreen)?;
        terminal.clear()?;

        // Remove lock
        fs::remove_file(&lock_path).ok();

        if let Some(agent) = app.agents.get_mut(app.selected) {
            agent.locked = false;
        }
    }
    Ok(())
}

fn spawn_new_agent(app: &mut App, input: &str) -> Result<()> {
    // Parse input as "name: description" or just use as description with auto-name
    let (name, description) = if let Some((n, d)) = input.split_once(':') {
        (n.trim().to_string(), d.trim().to_string())
    } else {
        let name = format!("agent-{}", app.agents.len() + 1);
        (name, input.to_string())
    };

    // Create worktree
    let worktree_path = worktree::create_worktree(&app.project_dir, &name)?;

    // Create tmux pane
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

    app.status_line = format!("spawned agent '{}'", name);
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
        app.status_line = format!("relayed message to {} via orc", agent_name);
    }
    Ok(())
}

fn open_diff(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    if let Some(agent) = app.selected_agent() {
        let worktree = agent.worktree.to_str().unwrap().to_string();

        // Leave TUI temporarily
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

        let config_path = dirs::home_dir()
            .unwrap_or_default()
            .join(".orchestr8/lazygit.yml");

        let lazygit_cmd = format!(
            "lazygit --work-tree={} --git-dir={}/.git --use-config-file={}; exit",
            worktree,
            worktree,
            config_path.display()
        );
        let pane = app.session.create_pane(&worktree)?;
        tmux::send_keys(&pane, &lazygit_cmd)?;

        // Attach to see lazygit
        app.session.select_pane(&pane)?;
        app.session.attach()?;

        // Back from lazygit — clean up temp pane
        app.session.kill_pane(&pane).ok();

        // Resume TUI
        enable_raw_mode()?;
        execute!(terminal.backend_mut(), EnterAlternateScreen)?;
        terminal.clear()?;
    }
    Ok(())
}

fn ensure_lazygit_config() -> Result<()> {
    let config_dir = dirs::home_dir()
        .unwrap_or_default()
        .join(".orchestr8");
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
