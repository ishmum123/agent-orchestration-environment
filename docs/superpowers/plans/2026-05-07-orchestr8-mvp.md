# Orchestr8 MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a terminal tool in Rust that runs parallel Claude Code agents in tmux panes with an intelligent orchestrator (orc) that monitors, coordinates, and answers questions autonomously.

**Architecture:** A Rust CLI binary manages tmux lifecycle, git worktrees, and a ratatui-based dashboard. The orc is a Claude Code instance in tmux pane 0 with a generated CLAUDE.md that tells it how to monitor/coordinate agents via tmux commands. CLI communicates with orc via send-keys, reads agent panes for state detection, and handles all keybinds.

**Tech Stack:** Rust, ratatui + crossterm (TUI), clap (CLI), serde + serde_json (config), anyhow (errors). No async runtime — synchronous event loop with crossterm polling.

---

## File Structure

```
orchestr8/
  Cargo.toml
  src/
    main.rs           — CLI entry point (clap), terminal setup, event loop
    app.rs            — App state machine, key handling, agent polling
    tmux.rs           — tmux command wrappers (session, pane, capture, send)
    worktree.rs       — git worktree create/remove
    agent.rs          — Agent struct, AgentState, spawning
    orc.rs            — Orc CLAUDE.md generation, spawning, relay
    ui.rs             — Dashboard rendering (ratatui widgets)
  CLAUDE.md           — Project development instructions
```

---

## Task 1: Project Scaffolding

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/app.rs`
- Create: `src/tmux.rs`
- Create: `src/worktree.rs`
- Create: `src/agent.rs`
- Create: `src/orc.rs`
- Create: `src/ui.rs`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "orchestr8"
version = "0.1.0"
edition = "2021"
description = "Parallel Claude Code agents with an intelligent orchestrator"

[dependencies]
ratatui = "0.29"
crossterm = "0.28"
clap = { version = "4", features = ["derive"] }
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

- [ ] **Step 2: Create src/main.rs with CLI skeleton**

```rust
use anyhow::Result;
use clap::Parser;

mod app;
mod tmux;
mod worktree;
mod agent;
mod orc;
mod ui;

#[derive(Parser)]
#[command(name = "orchestr8", about = "Parallel Claude Code agents with intelligent orchestration")]
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
    println!("orchestr8 - project: {}, session: {}", cli.project, cli.session);
    Ok(())
}
```

- [ ] **Step 3: Create stub modules**

`src/app.rs`:
```rust
pub struct App;
```

`src/tmux.rs`:
```rust
// tmux session and pane management
```

`src/worktree.rs`:
```rust
// git worktree management
```

`src/agent.rs`:
```rust
// agent lifecycle and state
```

`src/orc.rs`:
```rust
// orchestrator management
```

`src/ui.rs`:
```rust
// dashboard rendering
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build`
Expected: compiles with no errors (warnings about unused modules are fine)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/
git commit -m "feat: project scaffolding with CLI and module stubs"
```

---

## Task 2: Tmux Module

**Files:**
- Modify: `src/tmux.rs`

- [ ] **Step 1: Implement TmuxSession and TmuxPane**

```rust
use anyhow::{Context, Result};
use std::process::Command;

/// Represents a tmux session managed by orchestr8
pub struct TmuxSession {
    pub name: String,
}

/// Identifies a specific pane within a tmux session
#[derive(Debug, Clone)]
pub struct TmuxPane {
    pub session: String,
    pub window: u32,
    pub pane: u32,
}

impl TmuxPane {
    pub fn target(&self) -> String {
        format!("{}:{}.{}", self.session, self.window, self.pane)
    }
}

impl TmuxSession {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string() }
    }

    /// Create a new detached tmux session
    pub fn create(&self) -> Result<()> {
        let status = Command::new("tmux")
            .args(["new-session", "-d", "-s", &self.name, "-x", "200", "-y", "50"])
            .status()
            .context("failed to create tmux session")?;
        if !status.success() {
            anyhow::bail!("tmux new-session failed with {}", status);
        }
        Ok(())
    }

    /// Kill the tmux session
    pub fn kill(&self) -> Result<()> {
        Command::new("tmux")
            .args(["kill-session", "-t", &self.name])
            .status()
            .context("failed to kill tmux session")?;
        Ok(())
    }

    /// Check if session exists
    pub fn exists(&self) -> bool {
        Command::new("tmux")
            .args(["has-session", "-t", &self.name])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Create a new pane by splitting, returns the pane id
    pub fn create_pane(&self, working_dir: &str) -> Result<TmuxPane> {
        let output = Command::new("tmux")
            .args([
                "split-window", "-t", &self.name,
                "-c", working_dir,
                "-d",            // don't switch to it
                "-P",            // print pane info
                "-F", "#{pane_index}",
            ])
            .output()
            .context("failed to split tmux window")?;
        if !output.status.success() {
            anyhow::bail!("tmux split-window failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        let pane_index: u32 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .context("failed to parse pane index")?;
        Ok(TmuxPane {
            session: self.name.clone(),
            window: 0,
            pane: pane_index,
        })
    }

    /// Kill a specific pane
    pub fn kill_pane(&self, pane: &TmuxPane) -> Result<()> {
        Command::new("tmux")
            .args(["kill-pane", "-t", &pane.target()])
            .status()
            .context("failed to kill pane")?;
        Ok(())
    }

    /// Attach the user's terminal to this session
    pub fn attach(&self) -> Result<()> {
        let status = Command::new("tmux")
            .args(["attach-session", "-t", &self.name])
            .status()
            .context("failed to attach to tmux session")?;
        if !status.success() {
            anyhow::bail!("tmux attach failed with {}", status);
        }
        Ok(())
    }

    /// Select a specific pane (for pre-attach targeting)
    pub fn select_pane(&self, pane: &TmuxPane) -> Result<()> {
        Command::new("tmux")
            .args(["select-pane", "-t", &pane.target()])
            .status()
            .context("failed to select pane")?;
        Ok(())
    }

    /// Get the number of panes in window 0
    pub fn pane_count(&self) -> Result<u32> {
        let output = Command::new("tmux")
            .args([
                "list-panes", "-t", &format!("{}:0", self.name),
                "-F", "#{pane_index}",
            ])
            .output()
            .context("failed to list panes")?;
        let count = String::from_utf8_lossy(&output.stdout)
            .lines()
            .count() as u32;
        Ok(count)
    }
}

/// Send keystrokes to a tmux pane
pub fn send_keys(pane: &TmuxPane, text: &str) -> Result<()> {
    Command::new("tmux")
        .args(["send-keys", "-t", &pane.target(), text, "Enter"])
        .status()
        .context("failed to send keys to pane")?;
    Ok(())
}

/// Send keystrokes without pressing Enter
pub fn send_keys_raw(pane: &TmuxPane, text: &str) -> Result<()> {
    Command::new("tmux")
        .args(["send-keys", "-t", &pane.target(), text])
        .status()
        .context("failed to send raw keys to pane")?;
    Ok(())
}

/// Capture the last N lines of a pane's output
pub fn capture_pane(pane: &TmuxPane, lines: i32) -> Result<String> {
    let output = Command::new("tmux")
        .args([
            "capture-pane", "-t", &pane.target(),
            "-p",           // print to stdout
            "-J",           // join wrapped lines
            "-S", &format!("-{}", lines),
        ])
        .output()
        .context("failed to capture pane")?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build`
Expected: compiles successfully

- [ ] **Step 3: Commit**

```bash
git add src/tmux.rs
git commit -m "feat: tmux session and pane management module"
```

---

## Task 3: Git Worktree Module

**Files:**
- Modify: `src/worktree.rs`

- [ ] **Step 1: Implement worktree management**

```rust
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::fs;

/// Get the root of the current git repository
pub fn repo_root(project_dir: &str) -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(project_dir)
        .output()
        .context("failed to get repo root")?;
    if !output.status.success() {
        anyhow::bail!("not a git repository: {}", project_dir);
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(PathBuf::from(root))
}

/// Get the main branch name (main or master)
pub fn main_branch(project_dir: &str) -> Result<String> {
    // Try 'main' first
    let check = Command::new("git")
        .args(["rev-parse", "--verify", "main"])
        .current_dir(project_dir)
        .output()?;
    if check.status.success() {
        return Ok("main".to_string());
    }
    // Fall back to 'master'
    let check = Command::new("git")
        .args(["rev-parse", "--verify", "master"])
        .current_dir(project_dir)
        .output()?;
    if check.status.success() {
        return Ok("master".to_string());
    }
    anyhow::bail!("could not find main or master branch")
}

/// Create a git worktree for an agent
/// Returns the path to the worktree
pub fn create_worktree(project_dir: &str, agent_name: &str) -> Result<PathBuf> {
    let root = repo_root(project_dir)?;
    let base = main_branch(project_dir)?;
    let branch_name = format!("orchestr8/{}", agent_name);
    let worktree_dir = root
        .parent()
        .unwrap_or(Path::new("/tmp"))
        .join(format!(".orchestr8-worktrees/{}", agent_name));

    // Create parent directory
    if let Some(parent) = worktree_dir.parent() {
        fs::create_dir_all(parent)
            .context("failed to create worktree parent directory")?;
    }

    let status = Command::new("git")
        .args([
            "worktree", "add",
            "-b", &branch_name,
            worktree_dir.to_str().unwrap(),
            &base,
        ])
        .current_dir(project_dir)
        .status()
        .context("failed to create worktree")?;

    if !status.success() {
        anyhow::bail!("git worktree add failed for agent '{}'", agent_name);
    }

    Ok(worktree_dir)
}

/// Remove a git worktree and its branch
pub fn remove_worktree(project_dir: &str, agent_name: &str) -> Result<()> {
    let root = repo_root(project_dir)?;
    let worktree_dir = root
        .parent()
        .unwrap_or(Path::new("/tmp"))
        .join(format!(".orchestr8-worktrees/{}", agent_name));
    let branch_name = format!("orchestr8/{}", agent_name);

    // Remove the worktree
    Command::new("git")
        .args(["worktree", "remove", "--force", worktree_dir.to_str().unwrap()])
        .current_dir(project_dir)
        .status()
        .context("failed to remove worktree")?;

    // Delete the branch
    Command::new("git")
        .args(["branch", "-D", &branch_name])
        .current_dir(project_dir)
        .status()
        .context("failed to delete branch")?;

    Ok(())
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build`
Expected: compiles successfully

- [ ] **Step 3: Commit**

```bash
git add src/worktree.rs
git commit -m "feat: git worktree create/remove for agent isolation"
```

---

## Task 4: Agent Module

**Files:**
- Modify: `src/agent.rs`

- [ ] **Step 1: Define AgentState and Agent struct**

```rust
use crate::tmux::TmuxPane;
use std::path::PathBuf;
use std::time::Instant;

/// Agent lifecycle states shown on the dashboard
#[derive(Debug, Clone, PartialEq)]
pub enum AgentState {
    /// Agent is actively producing output
    Working,
    /// Agent asked something the orc deferred to the user
    WaitingForUser,
    /// Agent finished its current task
    Idle,
    /// Review passed, work is complete
    Done,
    /// Agent errored or looped, orc flagged it
    Stuck,
}

impl AgentState {
    pub fn label(&self) -> &str {
        match self {
            AgentState::Working => "working",
            AgentState::WaitingForUser => "waiting for you",
            AgentState::Idle => "idle",
            AgentState::Done => "done",
            AgentState::Stuck => "stuck",
        }
    }

    pub fn color(&self) -> ratatui::style::Color {
        use ratatui::style::Color;
        match self {
            AgentState::Working => Color::Green,
            AgentState::WaitingForUser => Color::Yellow,
            AgentState::Idle => Color::Gray,
            AgentState::Done => Color::Blue,
            AgentState::Stuck => Color::Red,
        }
    }
}

pub struct Agent {
    pub name: String,
    pub task_description: String,
    pub pane: TmuxPane,
    pub worktree: PathBuf,
    pub state: AgentState,
    pub last_output: String,
    pub locked: bool,
    pub last_poll: Instant,
}

impl Agent {
    pub fn new(
        name: String,
        task_description: String,
        pane: TmuxPane,
        worktree: PathBuf,
    ) -> Self {
        Self {
            name,
            task_description,
            pane,
            worktree,
            state: AgentState::Working,
            last_output: String::new(),
            locked: false,
            last_poll: Instant::now(),
        }
    }
}

/// Detect agent state from pane output.
/// Heuristic-based: looks for patterns in the last few lines.
pub fn detect_state(output: &str) -> AgentState {
    let lines: Vec<&str> = output
        .lines()
        .rev()
        .take(10)
        .collect();

    let recent = lines.join("\n").to_lowercase();

    // Check for waiting/question patterns (Claude Code shows a prompt)
    if recent.contains("? (y/n)")
        || recent.contains("do you want")
        || recent.contains("should i")
        || recent.contains("would you like")
    {
        return AgentState::WaitingForUser;
    }

    // Check for error/stuck patterns
    if recent.contains("error:")
        && recent.matches("error:").count() >= 2
    {
        return AgentState::Stuck;
    }

    // Check for completion patterns
    if recent.contains("task completed")
        || recent.contains("all done")
        || recent.contains("changes committed")
    {
        return AgentState::Done;
    }

    // Check for idle (Claude Code showing the input prompt with no activity)
    // The `>` prompt at the end with no recent streaming
    let trimmed = output.trim_end();
    if trimmed.ends_with('>')
        || trimmed.ends_with("$ ")
    {
        return AgentState::Idle;
    }

    AgentState::Working
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_working() {
        let output = "Reading file src/main.rs...\nAnalyzing code structure...\n";
        assert_eq!(detect_state(output), AgentState::Working);
    }

    #[test]
    fn test_detect_waiting() {
        let output = "I found two approaches. Should I use approach A or B? (y/n)\n";
        assert_eq!(detect_state(output), AgentState::WaitingForUser);
    }

    #[test]
    fn test_detect_stuck() {
        let output = "error: cannot find module\nerror: build failed\nerror: aborting\n";
        assert_eq!(detect_state(output), AgentState::Stuck);
    }

    #[test]
    fn test_detect_idle() {
        let output = "Done editing.\n> ";
        assert_eq!(detect_state(output), AgentState::Idle);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test`
Expected: all 4 tests pass

- [ ] **Step 3: Commit**

```bash
git add src/agent.rs
git commit -m "feat: agent struct and heuristic state detection"
```

---

## Task 5: App State Machine and Event Loop

**Files:**
- Modify: `src/app.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Implement App struct and mode enum**

`src/app.rs`:
```rust
use crate::agent::Agent;
use crate::tmux::{self, TmuxSession};
use anyhow::Result;
use std::time::Instant;

/// Dashboard interaction modes
pub enum AppMode {
    /// Viewing the dashboard, navigating agents
    Dashboard,
    /// Typing an inline prompt (for [t]ell or [n]ew)
    Input { prompt_label: String, callback: InputCallback },
    /// User is attached to a tmux pane — CLI is suspended
    Attached,
}

/// What to do with inline input after Enter
pub enum InputCallback {
    TellAgent,
    NewAgent,
}

pub struct App {
    pub session: TmuxSession,
    pub agents: Vec<Agent>,
    pub selected: usize,
    pub mode: AppMode,
    pub input_buf: String,
    pub should_quit: bool,
    pub project_dir: String,
    pub last_poll: Instant,
    pub status_line: String,
    pub orc_pane: Option<tmux::TmuxPane>,
}

impl App {
    pub fn new(session_name: &str, project_dir: &str) -> Self {
        Self {
            session: TmuxSession::new(session_name),
            agents: Vec::new(),
            selected: 0,
            mode: AppMode::Dashboard,
            input_buf: String::new(),
            should_quit: false,
            project_dir: project_dir.to_string(),
            last_poll: Instant::now(),
            status_line: String::new(),
            orc_pane: None,
        }
    }

    pub fn selected_agent(&self) -> Option<&Agent> {
        self.agents.get(self.selected)
    }

    pub fn select_next(&mut self) {
        if !self.agents.is_empty() {
            self.selected = (self.selected + 1) % self.agents.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.agents.is_empty() {
            self.selected = if self.selected == 0 {
                self.agents.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    /// Poll all agent panes for state updates
    pub fn poll_agents(&mut self) -> Result<()> {
        for agent in &mut self.agents {
            if agent.locked {
                continue;
            }
            let output = tmux::capture_pane(&agent.pane, 50)?;
            agent.state = crate::agent::detect_state(&output);
            agent.last_output = output;
            agent.last_poll = Instant::now();
        }
        self.last_poll = Instant::now();
        Ok(())
    }
}
```

- [ ] **Step 2: Update main.rs with terminal setup and event loop**

```rust
use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::time::{Duration, Instant};

mod agent;
mod app;
mod orc;
mod tmux;
mod ui;
mod worktree;

use app::{App, AppMode, InputCallback};

#[derive(Parser)]
#[command(name = "orchestr8", about = "Parallel Claude Code agents with intelligent orchestration")]
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
                    AppMode::Dashboard => handle_dashboard_key(app, key.code, key.modifiers)?,
                    AppMode::Input { .. } => handle_input_key(app, key.code)?,
                    AppMode::Attached => {} // shouldn't receive keys while attached
                }
            }
        }

        // Periodic agent polling
        if app.last_poll.elapsed() > poll_interval {
            app.poll_agents().ok(); // don't crash on poll failure
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn handle_dashboard_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => app.select_next(),
        KeyCode::Char('k') | KeyCode::Up => app.select_prev(),
        KeyCode::Char('a') | KeyCode::Enter => attach_to_agent(app)?,
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
                    prompt_label: format!(
                        "tell {}> ",
                        app.agents[app.selected].name
                    ),
                    callback: InputCallback::TellAgent,
                };
                app.input_buf.clear();
            }
        }
        KeyCode::Char('d') => open_diff(app)?,
        _ => {}
    }
    Ok(())
}

fn handle_input_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Enter => {
            let input = app.input_buf.clone();
            if input.is_empty() {
                app.mode = AppMode::Dashboard;
                return Ok(());
            }
            // Take the callback before changing mode
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

fn attach_to_agent(app: &mut App) -> Result<()> {
    if let Some(agent) = app.agents.get_mut(app.selected) {
        agent.locked = true;
        let pane = agent.pane.clone();

        // Leave TUI temporarily
        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen)?;

        // Select the agent's pane and attach
        app.session.select_pane(&pane)?;
        app.session.attach()?;

        // User detached — resume TUI
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;

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
    let pane = app.session.create_pane(worktree_path.to_str().unwrap())?;

    // Launch claude in the pane
    tmux::send_keys(&pane, "claude")?;

    // Wait a moment for claude to start, then send the prompt
    std::thread::sleep(Duration::from_secs(3));
    tmux::send_keys(&pane, &description)?;

    let agent = agent::Agent::new(name.clone(), description, pane, worktree_path);
    app.agents.push(agent);
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

fn open_diff(app: &mut App) -> Result<()> {
    if let Some(agent) = app.selected_agent() {
        let worktree = agent.worktree.to_str().unwrap();

        // Leave TUI temporarily
        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen)?;

        // Create a temp pane with lazygit
        let lazygit_cmd = format!(
            "lazygit --work-tree={} --git-dir={}/.git; exit",
            worktree, worktree
        );
        let pane = app.session.create_pane(worktree)?;
        tmux::send_keys(&pane, &lazygit_cmd)?;

        // Attach to see lazygit
        app.session.select_pane(&pane)?;
        app.session.attach()?;

        // Back from lazygit — clean up temp pane
        app.session.kill_pane(&pane).ok();

        // Resume TUI
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
    }
    Ok(())
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build`
Expected: compiles (may have warnings about unused orc/ui functions — that's fine, they're implemented next)

- [ ] **Step 4: Commit**

```bash
git add src/app.rs src/main.rs
git commit -m "feat: app state machine and event loop with keybinds"
```

---

## Task 6: Dashboard UI

**Files:**
- Modify: `src/ui.rs`

- [ ] **Step 1: Implement dashboard rendering**

```rust
use crate::app::{App, AppMode};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

pub fn render(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // header
            Constraint::Min(5),    // agent list
            Constraint::Length(3), // status bar / input
        ])
        .split(f.area());

    render_header(f, app, chunks[0]);
    render_agent_list(f, app, chunks[1]);
    render_bottom(f, app, chunks[2]);
}

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let working = app.agents.iter()
        .filter(|a| matches!(a.state, crate::agent::AgentState::Working))
        .count();
    let waiting = app.agents.iter()
        .filter(|a| matches!(a.state, crate::agent::AgentState::WaitingForUser))
        .count();

    let header = format!(
        "orchestr8 \u{2500} {} agents \u{2500} {} working \u{2500} {} waiting",
        app.agents.len(),
        working,
        waiting,
    );

    let block = Paragraph::new(header)
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::BOTTOM));

    f.render_widget(block, area);
}

fn render_agent_list(f: &mut Frame, app: &App, area: Rect) {
    if app.agents.is_empty() {
        let msg = Paragraph::new("No agents. Press [n] to spawn one.")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = app.agents.iter().enumerate().map(|(i, agent)| {
        let selected_marker = if i == app.selected { "\u{25b8} " } else { "  " };
        let state_label = agent.state.label();
        let state_color = agent.state.color();

        // Context bar — placeholder, will show real usage later
        let context_bar = "\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2593}\u{2593}";

        let line = Line::from(vec![
            Span::styled(
                selected_marker,
                Style::default().fg(Color::Green),
            ),
            Span::styled(
                format!("{:<16}", agent.name),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("[{}]", state_label),
                Style::default().fg(state_color),
            ),
            Span::raw("   "),
            Span::styled(
                truncate(&agent.task_description, 40),
                Style::default().fg(Color::Gray),
            ),
            Span::raw("   "),
            Span::styled(context_bar, Style::default().fg(Color::DarkGray)),
        ]);

        ListItem::new(line)
    }).collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE));

    f.render_widget(list, area);
}

fn render_bottom(f: &mut Frame, app: &App, area: Rect) {
    match &app.mode {
        AppMode::Input { prompt_label, .. } => {
            let input_text = format!("{}{}\u{2588}", prompt_label, app.input_buf);
            let input = Paragraph::new(input_text)
                .style(Style::default().fg(Color::Yellow))
                .block(Block::default().borders(Borders::TOP));
            f.render_widget(input, area);
        }
        _ => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Length(2)])
                .split(area);

            // Keybind hints
            let hints = Line::from(vec![
                Span::styled("[a]", Style::default().fg(Color::Green)),
                Span::raw("ttach "),
                Span::styled("[t]", Style::default().fg(Color::Green)),
                Span::raw("ell "),
                Span::styled("[d]", Style::default().fg(Color::Green)),
                Span::raw("iff "),
                Span::styled("[s]", Style::default().fg(Color::Green)),
                Span::raw("tatus "),
                Span::styled("[n]", Style::default().fg(Color::Green)),
                Span::raw("ew "),
                Span::styled("[q]", Style::default().fg(Color::Green)),
                Span::raw("uit"),
            ]);
            let hints_widget = Paragraph::new(hints)
                .block(Block::default().borders(Borders::TOP));
            f.render_widget(hints_widget, chunks[0]);

            // Status line
            if !app.status_line.is_empty() {
                let status = Paragraph::new(format!("last: {}", app.status_line))
                    .style(Style::default().fg(Color::DarkGray));
                f.render_widget(status, chunks[1]);
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}
```

- [ ] **Step 2: Verify it compiles and run briefly**

Run: `cargo build`
Expected: compiles successfully

- [ ] **Step 3: Commit**

```bash
git add src/ui.rs
git commit -m "feat: ratatui dashboard with agent list and keybind hints"
```

---

## Task 7: Orc Module

**Files:**
- Modify: `src/orc.rs`

- [ ] **Step 1: Implement orc CLAUDE.md generation and spawning**

```rust
use crate::tmux::{self, TmuxPane, TmuxSession};
use anyhow::Result;
use std::fs;
use std::path::Path;

/// Generate the CLAUDE.md content for the orchestrator
pub fn generate_orc_instructions(session_name: &str, project_dir: &str) -> String {
    format!(r#"# Orchestr8 Orchestrator

You are the orchestrator (orc) for an Orchestr8 session. You coordinate multiple Claude Code agents working in separate tmux panes on the same codebase.

## How to monitor agents

Read an agent's recent output:
```bash
tmux capture-pane -t {session}:0.{{PANE_ID}} -p -J -S -50
```

## How to talk to agents

Send a message to an agent:
```bash
tmux send-keys -t {session}:0.{{PANE_ID}} "your message here" Enter
```

## Your responsibilities

1. **Monitor** — every ~30 seconds, check each agent's pane output to track progress
2. **Answer** — if an agent asks a question you can answer from project context, answer it
3. **Redirect** — if an agent goes off-track (wrong files, looping errors), correct it
4. **Escalate** — if you can't resolve a problem, note it for the user

## Rules

- Before sending to any pane, check if file `~/.orchestr8/locked` exists. If it contains a pane id, do NOT send to that pane — the user is attached.
- Don't flood agents — one message at a time, wait for response
- Keep your answers concise and actionable
- Project directory: {project}

## Current agents

(Agents will be listed here as they are spawned. Check tmux panes for current state.)

Start monitoring now. Run `tmux list-panes -t {session}:0 -F '#{{pane_index}} #{{pane_current_command}}'` to see active panes.
"#,
        session = session_name,
        project = project_dir,
    )
}

/// Spawn the orchestrator Claude Code instance in the session's first pane (pane 0)
pub fn spawn_orc(session: &TmuxSession, project_dir: &str) -> Result<TmuxPane> {
    let orc_pane = TmuxPane {
        session: session.name.clone(),
        window: 0,
        pane: 0,
    };

    // Write orc instructions to a temp directory
    let orc_dir = dirs_or_tmp().join("orchestr8-orc");
    fs::create_dir_all(&orc_dir)?;

    let instructions = generate_orc_instructions(&session.name, project_dir);
    fs::write(orc_dir.join("CLAUDE.md"), instructions)?;

    // Launch claude in the orc pane (pane 0 already exists from session creation)
    let launch_cmd = format!(
        "cd {} && claude",
        orc_dir.display()
    );
    tmux::send_keys(&orc_pane, &launch_cmd)?;

    Ok(orc_pane)
}

/// Notify the orc about a new agent
pub fn notify_agent_spawned(orc_pane: &TmuxPane, agent_name: &str, pane_id: u32, task: &str) -> Result<()> {
    let msg = format!(
        "New agent spawned — name: \"{}\", pane id: {}, task: \"{}\". Add to your monitoring list.",
        agent_name, pane_id, task
    );
    tmux::send_keys(orc_pane, &msg)
}

fn dirs_or_tmp() -> std::path::PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".orchestr8"))
        .unwrap_or_else(|| Path::new("/tmp/.orchestr8").to_path_buf())
}
```

- [ ] **Step 2: Add `dirs` dependency to Cargo.toml**

Add to `[dependencies]`:
```toml
dirs = "6"
```

- [ ] **Step 3: Update spawn_new_agent in main.rs to notify orc**

In the `spawn_new_agent` function in `src/main.rs`, after the agent is pushed to `app.agents`, add:

```rust
    // Notify orc about the new agent
    if let Some(orc_pane) = &app.orc_pane {
        orc::notify_agent_spawned(orc_pane, &name, agent.pane.pane, &description)?;
    }
```

Note: `agent` has been moved into the vec, so capture the pane id and description before pushing. Restructure the end of `spawn_new_agent` to:

```rust
    let pane_id = pane.pane;
    let agent = agent::Agent::new(name.clone(), description.clone(), pane, worktree_path);
    app.agents.push(agent);

    // Notify orc about the new agent
    if let Some(orc_pane) = &app.orc_pane {
        orc::notify_agent_spawned(orc_pane, &name, pane_id, &description)?;
    }

    app.status_line = format!("spawned agent '{}'", name);
```

- [ ] **Step 4: Create the lock file mechanism**

Add to `src/main.rs` in `attach_to_agent`, before the attach call:

```rust
        // Write lock file so orc knows not to send to this pane
        let lock_path = dirs::home_dir()
            .unwrap_or_default()
            .join(".orchestr8/locked");
        fs::create_dir_all(lock_path.parent().unwrap())?;
        fs::write(&lock_path, pane.pane.to_string())?;
```

And after the attach returns (after resume TUI):

```rust
        // Remove lock
        let lock_path = dirs::home_dir()
            .unwrap_or_default()
            .join(".orchestr8/locked");
        fs::remove_file(&lock_path).ok();
```

Add `use std::fs;` to the top of main.rs if not already present.

- [ ] **Step 5: Verify it compiles**

Run: `cargo build`
Expected: compiles successfully

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml src/orc.rs src/main.rs
git commit -m "feat: orc spawning with CLAUDE.md generation and pane locking"
```

---

## Task 8: Lazygit Config and Session Cleanup

**Files:**
- Modify: `src/main.rs`
- Modify: `src/orc.rs` (reuse `dirs_or_tmp`)

- [ ] **Step 1: Generate lazygit config at startup**

Add a function to `src/main.rs` (or a new small module — inline in main is fine for now):

```rust
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
```

Call `ensure_lazygit_config()?;` near the top of `main()`.

- [ ] **Step 2: Update open_diff to use the config**

In `open_diff`, change the lazygit command to:

```rust
        let config_path = dirs::home_dir()
            .unwrap_or_default()
            .join(".orchestr8/lazygit.yml");
        let lazygit_cmd = format!(
            "lazygit --work-tree={} --git-dir={}/.git --use-config-file={}; exit",
            worktree, worktree, config_path.display()
        );
```

- [ ] **Step 3: Add graceful cleanup on Ctrl+C**

Add a `ctrlc` handler or just rely on the existing cleanup in `main()` after `run_loop` returns. The current cleanup code already handles this:

```rust
    // Cleanup (already in main)
    app.session.kill().ok();
    for agent in &app.agents {
        worktree::remove_worktree(&app.project_dir, &agent.name).ok();
    }
```

Verify the cleanup also removes the lock file and orc directory:

```rust
    // Additional cleanup
    let orchestr8_dir = dirs::home_dir().unwrap_or_default().join(".orchestr8");
    fs::remove_file(orchestr8_dir.join("locked")).ok();
    fs::remove_dir_all(orchestr8_dir.join("orchestr8-orc")).ok();
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build`
Expected: compiles successfully

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: lazygit config generation and graceful cleanup"
```

---

## Task 9: Status View ([s] keybind)

**Files:**
- Modify: `src/app.rs`
- Modify: `src/main.rs`
- Modify: `src/ui.rs`

- [ ] **Step 1: Add Status mode to AppMode**

In `src/app.rs`, add a variant to `AppMode`:

```rust
pub enum AppMode {
    Dashboard,
    Input { prompt_label: String, callback: InputCallback },
    Attached,
    /// Viewing layered status summaries
    Status,
}
```

- [ ] **Step 2: Add [s] keybind in main.rs**

In `handle_dashboard_key`, add:

```rust
        KeyCode::Char('s') => {
            app.mode = AppMode::Status;
        }
```

- [ ] **Step 3: Handle Esc in status mode**

In `run_loop`, update the key dispatch to handle Status mode:

```rust
                    match &app.mode {
                        AppMode::Dashboard => handle_dashboard_key(app, key.code, key.modifiers)?,
                        AppMode::Input { .. } => handle_input_key(app, key.code)?,
                        AppMode::Status => {
                            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('s')) {
                                app.mode = AppMode::Dashboard;
                            }
                        }
                        AppMode::Attached => {}
                    }
```

- [ ] **Step 4: Render status view in ui.rs**

Add a `render_status` function and call it from `render` when mode is Status:

In `render`:
```rust
pub fn render(f: &mut Frame, app: &App) {
    match &app.mode {
        AppMode::Status => render_status(f, app),
        _ => render_dashboard(f, app),
    }
}
```

Rename the existing `render` body to `render_dashboard` and add:

```rust
fn render_status(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(f.area());

    let header = Paragraph::new("Agent Status Summary")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::BOTTOM));
    f.render_widget(header, chunks[0]);

    let items: Vec<ListItem> = app.agents.iter().map(|agent| {
        // Get last few meaningful lines from output
        let summary = agent.last_output
            .lines()
            .rev()
            .filter(|l| !l.trim().is_empty())
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | ");
        let summary = truncate(&summary, 70);

        let lines = vec![
            Line::from(vec![
                Span::styled(
                    format!("{}: ", agent.name),
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("[{}]", agent.state.label()),
                    Style::default().fg(agent.state.color()),
                ),
            ]),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(summary, Style::default().fg(Color::Gray)),
            ]),
            Line::from(""),
        ];

        ListItem::new(lines)
    }).collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE));
    f.render_widget(list, chunks[1]);

    let hint = Paragraph::new("[Esc] back to dashboard")
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(hint, chunks[2]);
}
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo build`
Expected: compiles successfully

- [ ] **Step 6: Commit**

```bash
git add src/app.rs src/main.rs src/ui.rs
git commit -m "feat: status view with per-agent summaries"
```

---

## Task 10: CLAUDE.md and Final Polish

**Files:**
- Create: `CLAUDE.md`
- Create: `.gitignore`

- [ ] **Step 1: Create project CLAUDE.md**

```markdown
# Orchestr8

Rust TUI tool for running parallel Claude Code agents with an intelligent orchestrator.

## Build & Run

```bash
cargo build            # debug build
cargo build --release  # release build
cargo run              # run with defaults
cargo run -- -p /path/to/project -s my-session  # custom project dir and session name
cargo test             # run tests
```

## Architecture

- `src/main.rs` — CLI entry, terminal setup, event loop, keybind handlers
- `src/app.rs` — App state machine (Dashboard/Input/Attached/Status modes)
- `src/tmux.rs` — tmux session/pane management (create, kill, send-keys, capture-pane)
- `src/worktree.rs` — git worktree creation/removal for agent isolation
- `src/agent.rs` — Agent struct, AgentState enum, heuristic state detection
- `src/orc.rs` — Orchestrator CLAUDE.md generation and spawning
- `src/ui.rs` — Ratatui dashboard rendering

## Conventions

- No async runtime — synchronous event loop with crossterm polling
- Shell out to `tmux` and `git` via `std::process::Command` (no library bindings)
- Orc is a Claude Code instance with a generated CLAUDE.md, communicates via tmux
- Agent state detected from pane output heuristics, not IPC
```

- [ ] **Step 2: Create .gitignore**

```
/target
```

- [ ] **Step 3: Run full build and tests**

Run: `cargo build && cargo test`
Expected: builds and all tests pass

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md .gitignore
git commit -m "docs: add CLAUDE.md and .gitignore"
```

---

## Post-MVP Follow-ups (not in this plan)

These are captured for future plans, not implemented now:

1. **Orc task decomposition** — [n] sends description to orc for multi-agent decomposition instead of spawning one agent directly
2. **Review pass** — two-tier review (fast check + deep reviewer agent) when agent signals done
3. **Smart splitting** — orc detects context cap, spawns continuation agent
4. **Merging** — overlap detection between worktrees, merge agent for conflicts
5. **Checkpoints** — commit all worktrees + orc state to checkpoint branch
6. **Real context bar** — query Claude Code for actual token usage
7. **Editor view ([e])** — open $EDITOR at diff hunks in temp pane
8. **Layered summaries** — drill-down status with Enter/Esc navigation
9. **Multi-backend support** — trait-based AgentBackend for OpenCode, other models
