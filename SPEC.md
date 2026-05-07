# Orc

A terminal tool that wraps Claude Code with an intelligent orchestrator. You talk to orc like you'd talk to Claude Code. When work needs doing, orc spawns background agents and manages them — you only see them when they're running, and only intervene when orc can't handle it alone.

## Core principle

**Orc is Claude Code with a team.** You type, orc responds. The difference: when orc decides work needs doing, it delegates to background agents instead of doing it itself. Orc keeps its own context small and dedicated to understanding you.

## Architecture

```
┌─────────────────────────────────────────────────┐
│              Orc TUI (ratatui)                  │
│  ┌──────────────────┐  ┌─────────────────────┐  │
│  │   Orc Output     │  │  Agent Sidebar      │  │
│  │   (left panel)   │  │  ● agent-1    12s   │  │
│  │                  │  │  ✓ agent-2    45s   │  │
│  │                  │  │                     │  │
│  └──────────────────┘  └─────────────────────┘  │
│  ┌──────────────────────────────────────────┐   │
│  │ > user input                             │   │
│  └──────────────────────────────────────────┘   │
└─────────────────────────────────────────────────┘
        │                           │
        ▼                           ▼
  Orc Brain Process           Agent Processes
  (claude -p)                 (claude -p × N)
  - no tools                  - auto permissions
  - no MCP                    - own git worktree
  - reasons about tasks       - does actual work
  - emits [SPAWN_AGENT]
```

The TUI owns all process lifecycle. Claude Code instances are child processes communicating via `--input-format stream-json --output-format stream-json` NDJSON pipes. No tmux dependency.

## User experience flow

### 1. Launch

```
$ orc -p /path/to/project
```

Orc starts, greets you. You type naturally in the input bar.

### 2. Conversation — orc clarifies before acting

```
you: the login keeps re-prompting every 50 minutes and the edit/delete
     UI needs a redesign

orc: A few questions before I spin up agents:
     1. The 50-min re-prompt — session token expiring or re-auth flow?
     2. For edit/delete redesign — swipe-to-reveal or selection mode?
```

If orc understands clearly, it skips questions and spawns agents directly.

### 3. Agents spawn in the background

Orc embeds commands in its text output:

```
[SPAWN_AGENT name="auth-fix" task="Switch from ID token revalidation to session tokens in src/auth/"]
[SPAWN_AGENT name="edit-redesign" task="Implement swipe-to-reveal for edit/delete actions"]
```

The TUI parses these, creates git worktrees, spawns Claude Code processes, and sends each agent its task. Commands are stripped from displayed output — the user sees clean text.

### 4. Sidebar — visible when agents exist

```
┌──────────────────────────────────┬───────────────────┐
│                                  │ ● auth-fix    2m  │
│  orc conversation                │ ● edit-rede.. 2m  │
│  (you keep talking here)         │                   │
│                                  │                   │
└──────────────────────────────────┴───────────────────┘
```

Sidebar appears when agents are spawned. Each entry shows state icon, name, elapsed time.

### 5. Agent completion — feedback loop

When an agent finishes, the TUI sends the result back to the orc brain:

```
Agent "auth-fix" finished. Result: Replaced ID token revalidation...
```

Orc summarizes for the user. If the result triggers more work, orc can spawn additional agents.

### 6. Attention markers — orc escalates to you

When orc can't handle something, it tells you in the chat. You can respond to orc (who relays via `[TELL_AGENT]`) or send directly to the agent with `/`.

## Agent states

| Icon | State | Meaning |
|------|-------|---------|
| ● | working | Agent is actively producing output |
| ○ | needs you | Orc escalated, needs human decision |
| ─ | idle | Agent finished current task |
| ✓ | done | Work complete |
| ✗ | error | Process exited with error |

## Command protocol

Orc embeds commands in its natural language responses:

```
[SPAWN_AGENT name="short-slug" task="Full description of what to do"]
[TELL_AGENT name="agent-name" message="Message to send to the agent"]
[KILL_AGENT name="agent-name"]
```

Commands can span multiple lines (task/message values may contain newlines). The TUI parser extracts these, executes them, and strips them from displayed output.

## Process architecture

### Orc brain
- `claude -p --tools "" --disallowed-tools LSP --strict-mcp-config`
- System prompt instructs it to reason about tasks and emit commands
- Cannot run tools or edit files — pure reasoning
- Persistent for the session lifetime

### Worker agents
- `claude -p --permission-mode auto`
- Each in an isolated git worktree (branch `orc/{name}`, dir `../.orc-worktrees/{name}`)
- Stderr captured to `~/.orc/logs/{name}.stderr`
- Killed and cleaned up when done or on quit

### Communication
- All processes use `--input-format stream-json --output-format stream-json --verbose`
- TUI reads stdout as NDJSON (non-blocking), writes to stdin
- Event types: `system` (init), `assistant` (content blocks), `result` (completion)
- No polling — continuous non-blocking read in the event loop

## Keybinds

| Key | Action |
|-----|--------|
| esc | Chat mode (default — type to orc) |
| n | Spawn new agent manually |
| t | Tell selected agent (via orc enrichment) |
| / | Send directly to agent (bypass orc) |
| e | Open $EDITOR on agent's changed files |
| x | Kill selected agent (with confirmation) |
| s | Status overview |
| j/k | Navigate agents |
| Enter | Agent full output view |
| Ctrl+U/D | Scroll output |
| Tab | Toggle preview |
| ? | Help overlay |
| q / Ctrl+C | Quit (kills all, cleans up worktrees) |

## Worktree isolation

Each agent gets its own git worktree:

```
{repo_root_parent}/.orc-worktrees/
├── auth-fix/         # branch: orc/auth-fix
├── edit-redesign/    # branch: orc/edit-redesign
└── ...
```

On agent kill or orc quit: `git worktree remove --force` + `git branch -D`.

## File layout

```
~/.orc/
└── logs/
    ├── auth-fix.stderr
    └── edit-redesign.stderr

{repo_root_parent}/.orc-worktrees/
├── auth-fix/
└── edit-redesign/
```

## Non-goals (v1)

- Agents talking to each other (orc mediates all cross-agent coordination)
- Web UI or desktop app (terminal-first)
- Non-Claude agents (Claude Code only)
- Persisting across machine restarts
- API cost tracking (subscription-based usage assumed)
