# Orc

A terminal tool that wraps Claude Code with an intelligent orchestrator. You talk to orc like you'd talk to Claude Code. When work needs doing, orc spawns background agents and manages them — you only see them when they're running, and only intervene when orc can't handle it alone.

## Core principle

**Orc is Claude Code with a team.** The first time you open orc it should be indistinguishable from opening Claude Code. You type, orc responds. The difference: when orc decides work needs doing, it delegates to background agents instead of doing it itself. Orc keeps its own context small and dedicated to understanding you.

## Architecture

```
tmux session "orc"
├── pane 0: orc (Claude Code) ← user talks here, this IS the UI
├── pane 1: agent "auth-fix" (Claude Code, background)
│   └── own git worktree, invisible unless sidebar is open
├── pane 2: agent "page-builder" (Claude Code, background)
│   └── own git worktree, invisible unless sidebar is open
└── ...
```

The user's terminal shows pane 0 — the orc. Agents run in other panes but the user never sees them unless they choose to. A sidebar appears on the right when agents are active.

## User experience flow

### 1. Launch — indistinguishable from Claude Code

```
$ orc
```

You land in the orc's pane. It looks and feels like `claude`. You type naturally. No dashboard, no keybind bar, no empty agent list. Just a prompt.

### 2. Conversation — orc clarifies before acting

```
you: the login keeps re-prompting every 50 minutes and the edit/delete
     UI needs a redesign

orc: A few questions before I spin up agents:
     1. The 50-min re-prompt — is that the session token expiring or
        a re-auth flow? I see both patterns in src/auth/.
     2. For the edit/delete redesign, are you thinking swipe-to-reveal
        or a selection mode with a toolbar?
```

If orc understands the task clearly, it skips questions and goes straight to spawning. If not, it asks — just like a good colleague would.

### 3. Agents spawn in the background

Once orc knows what to do:

```
orc: Got it. Spinning up two agents:
     • auth-fix — switch from ID token revalidation to session tokens
     • edit-redesign — swipe-to-reveal with custom touch handler

     I'll manage them and let you know when they're done.
```

Orc creates git worktrees, tmux panes, and launches Claude Code in each. It crafts a scoped prompt per agent with file paths, constraints, and context. The agents start working immediately.

At this point a **sidebar appears on the right** showing agent status.

### 4. Sidebar — only visible when agents are running

```
┌─────────────────────────────────┬──────────────────┐
│                                 │ ● auth-fix    2m │
│  orc conversation               │ ● edit-rede.. 2m │
│  (you keep talking here)        │                  │
│                                 │                  │
│                                 │                  │
└─────────────────────────────────┴──────────────────┘
```

- Sidebar appears automatically when agents are spawned
- Sidebar disappears when all agents finish
- Each entry shows: state icon, name, elapsed time
- User can toggle sidebar visibility with a hotkey

### 5. Orc manages agents autonomously

While agents work, orc monitors them (~30s polling):

- **Answers questions** — agent asks "AppCompatActivity or ComponentActivity?" Orc knows the codebase uses AppCompatActivity, types the answer into the agent's pane.
- **Redirects** — agent starts editing wrong files, orc corrects it.
- **Splits work** — agent hitting context limits, orc spawns a continuation agent with the remaining work and key decisions from the original.

The user doesn't see any of this unless they open the sidebar or the agent's pane.

### 6. Attention markers — orc escalates to you

When orc genuinely can't handle something, it marks the agent:

```
┌─────────────────────────────────┬──────────────────┐
│                                 │ ○ auth-fix    5m │
│  orc: auth-fix needs your       │ ● edit-rede.. 5m │
│  input — it wants to change     │                  │
│  the token format from JWT to   │                  │
│  opaque, which affects the      │                  │
│  mobile app. Should it proceed? │                  │
│                                 │                  │
└─────────────────────────────────┴──────────────────┘
```

Orc does two things:
1. Marks the agent with ○ (needs attention) in the sidebar
2. Tells the user in the chat what happened and what decision is needed

The user can either:
- Answer orc in the chat (orc relays to agent)
- Open the agent's pane directly to handle it themselves

Escalation triggers:
- Agent asks a question orc can't confidently answer
- Agent wants to do something destructive or outside scope
- Agent is stuck (looping errors, can't make progress)
- Agent made an architectural decision that should be confirmed

### 7. Opening an agent's pane

From the sidebar, the user can select an agent to jump into its pane:

```
hotkey → select agent → full-screen agent pane (tmux attach)
Esc/detach → back to orc
```

While the user is in an agent's pane, orc doesn't send to that pane (lock file). When the user detaches, orc resumes management.

### 8. Agents finish — sidebar disappears

When all agents complete:

```
orc: Both agents are done.
     • auth-fix — replaced ID token revalidation with session tokens.
       3 files changed. Tests pass.
     • edit-redesign — implemented swipe-to-reveal with custom
       OnTouchListener. Hit a snag with ItemTouchHelper, switched
       approach. 5 files changed.

     Want me to show the diffs or merge them?
```

Sidebar goes away. You're back to a clean Claude Code experience.

## Agent states

- **● working** (green) — agent is actively producing output
- **○ needs you** (yellow) — orc escalated, needs human decision
- **─ idle** (gray) — agent finished its current task
- **✓ done** (cyan) — work complete, review passed
- **✗ stuck** (red) — errors/looping, orc flagged it

## Spawn protocol

Orc can't directly create tmux panes or git worktrees — it's a Claude Code instance. Instead:

1. Orc writes a spawn request to `~/.orc/spawn-queue.json`:
   ```json
   [
     {"name": "auth-fix", "task": "Replace ID token revalidation with session tokens in src/auth/"},
     {"name": "edit-redesign", "task": "Implement swipe-to-reveal for edit/delete actions"}
   ]
   ```

2. The CLI polls this file every 2 seconds. When it finds requests:
   - Creates git worktree per agent
   - Creates tmux pane per agent
   - Launches `claude` in each pane
   - Sends the task prompt
   - Notifies orc that agents are live (with pane IDs)
   - Deletes the processed requests

3. Orc receives confirmation and begins monitoring.

Similarly, orc can write kill requests to `~/.orc/kill-queue.json` to tear down agents it considers done.

## Orc's CLAUDE.md

Generated at startup. Instructs orc to:

- Act as the user's primary interface — conversational, helpful, like Claude Code
- Never attempt implementation work itself — always delegate to agents
- Keep context small: monitor agent panes (last ~50 lines), don't absorb full transcripts
- Clarify ambiguous requests before spawning agents
- Spawn agents via the queue file protocol
- Monitor agents every ~30 seconds via `tmux capture-pane`
- Answer agent questions when confident, escalate when not
- Report agent completion with summaries
- Use the lock file protocol to avoid sending to user-occupied panes

## Keybinds

Minimal — the user is in orc's pane most of the time.

| Key | Action |
|-----|--------|
| Ctrl+O | Toggle sidebar visibility |
| Ctrl+] | Select next agent in sidebar |
| Ctrl+[ | Select prev agent in sidebar |
| Enter (with agent selected) | Open agent's pane (full tmux attach) |
| Esc / detach | Return to orc from agent pane |

All other interaction happens through conversation with orc.

## Context management

Orc manages agent context actively:

- **Scoped initial prompts** — agents get file paths, constraints, and relevant context, not the whole codebase
- **Monitoring** — orc reads agent panes every ~30 seconds to track progress
- **Compaction** — orc tells agent to /compact when approaching context cap
- **Smart splitting** — when an agent runs out of room, orc spawns a continuation agent with remaining work and key decisions (not a transcript copy)

## Review and completion

When an agent signals done:

1. **Fast check** — orc reads the agent's pane output. Did it touch the right files? Does the diff look proportional? Did it say it's done?
2. **Deep review** — for large diffs or architectural decisions, orc spawns a short-lived reviewer agent with the original task + diff. Reviewer returns verdict, orc acts on it.
3. **Report** — orc tells the user what was accomplished and asks about next steps.

## Merging

Agents work in separate git worktrees. When ready to integrate:

1. Orc checks for file overlap between completed agents
2. No overlap → fast-forward merge
3. Overlap → orc spawns a short-lived merge agent with both diffs
4. Conflict it can't resolve → escalates to user

The user can also manually merge via lazygit or by attaching to the main repo.

## File layout

```
~/.orc/
├── orc/              # orc's working directory with generated CLAUDE.md
├── locked            # contains pane ID if user is attached to an agent
├── spawn-queue.json  # orc writes spawn requests here
├── kill-queue.json   # orc writes kill requests here
└── lazygit.yml       # generated lazygit config for diff viewing

{repo_root_parent}/.orc-worktrees/
├── auth-fix/         # agent worktree (branch: orc/auth-fix)
├── edit-redesign/    # agent worktree (branch: orc/edit-redesign)
└── ...
```

## Non-goals (v1)

- Agents talking to each other (orc mediates all cross-agent coordination)
- Web UI or desktop app (terminal-first)
- Non-Claude agents (Claude Code only)
- Persisting across machine restarts
- Restoring orc state from checkpoints
- User interacting with sub-agents (agents can spawn their own sub-agents, invisible to user)
