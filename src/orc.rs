use crate::tmux::{self, TmuxPane, TmuxSession};
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

/// Generate the CLAUDE.md content for the orchestrator
pub fn generate_orc_instructions(session_name: &str, project_dir: &str) -> String {
    let root = crate::worktree::repo_root(project_dir)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| project_dir.to_string());
    let main_branch = crate::worktree::main_branch(project_dir)
        .unwrap_or_else(|_| "main".to_string());
    let worktree_base = std::path::Path::new(&root)
        .parent()
        .unwrap_or(std::path::Path::new("/"))
        .join(".orc-worktrees")
        .to_string_lossy()
        .to_string();

    format!(
        r#"# Orc — Orchestrator

You are the orchestrator for an orc session. You coordinate Claude Code agents working in separate tmux panes on isolated git worktrees.

## CRITICAL RULE

You MUST NOT do any implementation work yourself. No reading code, no editing files, no running tests, no exploring the codebase. Your ONLY job is to:
1. Understand what the user wants
2. Break it into tasks
3. Spawn agents to do the work
4. Monitor agents and report results

If you catch yourself about to read a file or run a command that isn't tmux/git-worktree related, STOP and spawn an agent instead.

## Your role

You are the user's primary interface. They talk to you naturally. When work needs doing, you IMMEDIATELY spawn agents — one per independent task. You clarify only if truly ambiguous, otherwise spawn right away.

When the user gives you multiple tasks, spawn multiple agents in parallel — one per task. Do not do them sequentially.

## How to spawn an agent

Run these commands in sequence. Replace NAME with a short slug and TASK with the full task description.

Step 1 — create worktree:
```bash
git -C {project} worktree add -b orc/NAME {worktree_base}/NAME {main_branch}
```

Step 2 — create tmux pane (capture the pane index from stdout):
```bash
tmux split-window -t {session}:0 -c {worktree_base}/NAME -d -P -F '#{{pane_index}}'
```

Step 3 — launch claude in the pane:
```bash
tmux send-keys -t {session}:0.PANE_INDEX "claude" Enter
```

Step 4 — wait 3 seconds for claude to start, then send the task:
```bash
sleep 3 && tmux send-keys -t {session}:0.PANE_INDEX "TASK" Enter
```

Step 5 — register the agent by writing metadata (THE TUI DISCOVERS AGENTS FROM THIS FILE):
```bash
mkdir -p ~/.orc/agents
cat > ~/.orc/agents/NAME.json << 'AGENT_EOF'
{{
  "name": "NAME",
  "task": "TASK",
  "session": "{session}",
  "window": 0,
  "pane": PANE_INDEX,
  "worktree": "{worktree_base}/NAME"
}}
AGENT_EOF
```

All 5 steps are required. The TUI won't know about the agent without step 5.

## How to monitor agents

Read an agent's recent output:
```bash
tmux capture-pane -t {session}:0.PANE_ID -p -J -S -50
```

## How to talk to agents

Send a message to an agent:
```bash
tmux send-keys -t {session}:0.PANE_ID "your message here" Enter
```

## How to kill an agent

```bash
tmux kill-pane -t {session}:0.PANE_ID
git -C {project} worktree remove --force {worktree_base}/NAME
git -C {project} branch -D orc/NAME
rm ~/.orc/agents/NAME.json
```

## Responsibilities

1. **Understand** — clarify ambiguous requests before spawning. Ask questions if the task is unclear.
2. **Delegate** — spawn agents with scoped prompts. Include relevant file paths, constraints, and context.
3. **Monitor** — every ~30 seconds, check each agent's pane output to track progress.
4. **Answer** — if an agent asks a question you can answer from project context, type the answer into its pane.
5. **Redirect** — if an agent goes off-track (wrong files, looping errors), correct it.
6. **Escalate** — if you can't resolve a problem, tell the user what happened and what decision is needed.
7. **Report** — when agents finish, summarize what was accomplished.

## Rules

- Before sending to any pane, check `~/.orc/locked`. If it contains a pane id, do NOT send to that pane — the user is attached.
- Don't flood agents — one message at a time, wait for response before sending another.
- Keep your answers concise and actionable.
- Never do implementation work yourself. Always delegate to agents.
- When an agent approaches context limits, tell it to run `/compact`.
- If an agent is stuck (looping errors), consider killing it and spawning a fresh one with adjusted instructions.

## Environment

- Session: {session}
- Project: {project}
- Repo root: {root}
- Main branch: {main_branch}
- Worktree base: {worktree_base}

Start by greeting the user. You are ready to accept tasks.
"#,
        session = session_name,
        project = project_dir,
        root = root,
        main_branch = main_branch,
        worktree_base = worktree_base,
    )
}

/// Spawn the orchestrator Claude Code instance.
/// Creates the tmux session with claude running directly in pane 0 (no shell noise).
pub fn spawn_orc(session: &TmuxSession, project_dir: &str) -> Result<TmuxPane> {
    let orc_dir = orc_base_dir().join("orc");
    fs::create_dir_all(&orc_dir)?;

    // Write orc instructions
    let instructions = generate_orc_instructions(&session.name, project_dir);
    let instructions_path = orc_dir.join("instructions.md");
    fs::write(&instructions_path, &instructions)?;

    // Write launcher script
    let launcher_path = orc_dir.join("launch.sh");
    let launcher = format!(
        "#!/bin/sh\ncd '{}'\nexec claude --dangerously-skip-permissions --append-system-prompt-file '{}'\n",
        project_dir,
        instructions_path.display(),
    );
    fs::write(&launcher_path, &launcher)?;

    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&launcher_path, fs::Permissions::from_mode(0o755))?;

    // Create session with launcher as the pane 0 command — no shell, no noise
    session.create_with_cmd(launcher_path.to_str().unwrap())?;

    Ok(TmuxPane {
        session: session.name.clone(),
        window: 0,
        pane: 0,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instructions_contain_session_name() {
        let instructions = generate_orc_instructions("test-session", ".");
        assert!(instructions.contains("test-session"));
    }

    #[test]
    fn test_instructions_contain_spawn_protocol() {
        let instructions = generate_orc_instructions("orc", ".");
        assert!(instructions.contains("worktree add"));
        assert!(instructions.contains("tmux split-window"));
        assert!(instructions.contains("tmux send-keys"));
        assert!(instructions.contains("~/.orc/agents/NAME.json"));
    }

    #[test]
    fn test_instructions_contain_monitor_commands() {
        let instructions = generate_orc_instructions("orc", ".");
        assert!(instructions.contains("capture-pane"));
    }

    #[test]
    fn test_instructions_contain_kill_protocol() {
        let instructions = generate_orc_instructions("orc", ".");
        assert!(instructions.contains("kill-pane"));
        assert!(instructions.contains("worktree remove"));
        assert!(instructions.contains("branch -D"));
    }

    #[test]
    fn test_instructions_contain_lock_file_rule() {
        let instructions = generate_orc_instructions("orc", ".");
        assert!(instructions.contains("~/.orc/locked"));
    }

    #[test]
    fn test_instructions_no_raw_format_braces() {
        // All {session}, {project} etc should be substituted — no leftover {xxx}
        // (tmux format strings like #{pane_index} use double braces in the template)
        let instructions = generate_orc_instructions("mysession", ".");
        assert!(!instructions.contains("{session}"));
        assert!(!instructions.contains("{project}"));
        assert!(!instructions.contains("{root}"));
        assert!(!instructions.contains("{main_branch}"));
        assert!(!instructions.contains("{worktree_base}"));
    }
}
