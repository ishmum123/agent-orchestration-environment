use crate::tmux::{self, TmuxPane, TmuxSession};
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

/// Generate the CLAUDE.md content for the orchestrator
pub fn generate_orc_instructions(session_name: &str, project_dir: &str) -> String {
    format!(
        r#"# Orchestr8 Orchestrator

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

- Before sending to any pane, check if file `~/.orc/locked` exists. If it contains a pane id, do NOT send to that pane — the user is attached.
- Don't flood agents — one message at a time, wait for response.
- Keep your answers concise and actionable.
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

    // Write orc instructions to a dedicated directory
    let orc_dir = orc_base_dir().join("orc");
    fs::create_dir_all(&orc_dir)?;

    let instructions = generate_orc_instructions(&session.name, project_dir);
    fs::write(orc_dir.join("CLAUDE.md"), instructions)?;

    // Launch claude in the orc pane (pane 0 already exists from session creation)
    let launch_cmd = format!("cd {} && claude", orc_dir.display());
    tmux::send_keys(&orc_pane, &launch_cmd)?;

    Ok(orc_pane)
}

/// Notify the orc about a new agent
pub fn notify_agent_spawned(
    orc_pane: &TmuxPane,
    agent_name: &str,
    pane_id: u32,
    task: &str,
) -> Result<()> {
    let msg = format!(
        "New agent spawned — name: \"{}\", pane id: {}, task: \"{}\". Add to your monitoring list.",
        agent_name, pane_id, task
    );
    tmux::send_keys(orc_pane, &msg)
}

fn orc_base_dir() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".orc"))
        .unwrap_or_else(|| Path::new("/tmp/.orc").to_path_buf())
}
