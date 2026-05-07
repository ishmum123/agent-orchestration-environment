# Orchestr8

A terminal tool that runs parallel Claude Code agents with an intelligent orchestrator. You talk to agents when you want. The orchestrator handles them when you don't.

## The gap

**Agent Teams:** smart orchestrator, but you can't naturally talk to individual agents.
**Claude Squad:** great direct-talk UX, but no intelligent coordination — you do everything.
**Orchestr8:** both. Smart lead + direct talk + autonomous coordination.

## How it works

The orchestrator is a Claude Code session running in a tmux pane. It spawns agents as separate Claude Code sessions in other tmux panes. It talks to them by typing into their panes (`tmux send-keys`) and reads their output (`tmux capture-pane`). Agents can't tell whether you or the orchestrator is typing — it's the same terminal.

```
┌─────────────────────────────────────────────┐
│  Dashboard (orchestrator pane)              │
│                                             │
│  agent-1: auth-fix      [working]           │
│  agent-2: page-builder  [waiting for you]   │
│  agent-3: ui-redesign   [idle]              │
│                                             │
│  > tell agent-1 to use session tokens       │
│  > attach agent-2                           │
└─────────────────────────────────────────────┘
```

## Architecture

```
tmux session "orchestr8"
├── pane 0: orchestrator (Claude Code)
│   ├── reads agent panes via capture-pane
│   ├── writes to agent panes via send-keys
│   └── holds the plan, not the transcripts
├── pane 1: agent "auth-fix" (Claude Code)
│   └── own git worktree, own context
├── pane 2: agent "page-builder" (Claude Code)
│   └── own git worktree, own context
└── pane 3: agent "ui-redesign" (Claude Code)
    └── own git worktree, own context
```

All intelligence is Claude Code. The only non-Claude-Code piece is a thin CLI that manages tmux lifecycle and keyboard routing.

## Three ways to talk to an agent

### 1. Direct — you attach to the agent's pane

```
you: attach agent-2
# tmux switches you to pane 2
# you type directly into agent-2's Claude Code session
# orchestrator stops sending to this pane
you: detach
# back to dashboard, orchestrator can resume
```

The agent sees a human typing. That's all it ever sees.

### 2. Through orchestrator — you tell the orchestrator what to relay

```
you: tell agent-1 to try session tokens instead of re-validating the ID token every call
```

The orchestrator doesn't just parrot this. It adds context:
- "The server-side is in Code.gs, the token validation is on line 24-25"
- "The user wants you to issue your own session token after first verification"
- "This relates to the 50-minute re-prompt bug"

Then types the full message into agent-1's pane.

### 3. Orchestrator independent — no input from you needed

Agent-2 asks: "Should AccountActivity extend AppCompatActivity or ComponentActivity?"

You're not there. The orchestrator reads this from the pane, knows the project uses AppCompatActivity everywhere, types the answer. You never see the question unless you check the logs.

## Spawning agents

You describe what you want at a high level:

```
you: the login keeps re-prompting every 50 minutes. the account/chat
     dialogs need to be full pages. the edit/delete UI needs a redesign.
```

The orchestrator:
1. Explores the codebase (or uses context you've given it)
2. Breaks the work into scoped tasks — not by file count, by problem seams
3. Crafts a focused prompt for each agent with specific file paths, constraints, design direction
4. Spawns each agent in its own tmux pane + git worktree
5. Types the prompt into each pane

The agents start smart, not cold.

## Context cap: 50k tokens per agent

The orchestrator enforces this. Techniques:

- **Scoped initial prompts** — agents get what they need, not the whole codebase
- **Monitoring** — orchestrator periodically captures pane output to gauge how deep the agent is
- **Compaction** — orchestrator tells the agent to /compact when approaching the cap
- **Smart splitting** — when an agent is running out of room:

```
orchestrator detects agent-2 at ~45k tokens
orchestrator reads agent-2's pane to understand:
  - what it accomplished (AccountActivity done, ChatActivity half-done)
  - what decisions it made (chose Activities over Fragments, uses existing theme)
  - what remains (ChatActivity layout, bubble adapters, task picker)

orchestrator spawns agent-4 "chat-builder" with:
  - the key decisions from agent-2 (not the transcript)
  - the remaining work, scoped precisely
  - links to what agent-2 already created

agent-2 is told to wrap up and finalize its completed work
```

This is not "copy the prompt and restart." The orchestrator understands what's done and what's left, then designs a continuation task around the actual remaining problem.

## Sub-agents

Agents can spawn their own sub-agents for internal parallelization. Max 2 layers deep:

```
Orchestrator (layer 0)
└── Agent (layer 1) — visible on dashboard, you can talk to it
    └── Sub-agent (layer 2) — invisible to you, agent manages it
```

Sub-agents never appear on the dashboard. You never talk to them. The agent that spawned them is responsible for their output. If agent-2 wants to parallelize XML layouts and Kotlin code, that's its business.

## Dashboard

The orchestrator's pane is the dashboard. It shows:

```
orchestr8 — 3 agents active

  auth-fix       [working]          added setAutoSelectEnabled(true)
  page-builder   [waiting for you]  "should chat use ViewPager2?"
  ui-redesign    [working]          implementing swipe-to-reveal

  [a]ttach  [t]ell  [s]tatus  [d]iff  [n]ew agent
```

- **working** — agent is actively producing output
- **waiting for you** — agent asked a question the orchestrator deferred to you
- **idle** — agent finished its current task
- **stuck** — agent errored or looped, orchestrator flagged it

The orchestrator decides what to surface to you vs handle itself. Routine questions it answers. Judgment calls it escalates.

## User modes

**Directing** — attached to an agent, typing directly. Orchestrator doesn't interfere.

**Supervising** — on the dashboard. You see status, review diffs, tell the orchestrator to relay messages. Orchestrator handles the rest.

**Away** — you walked away. Orchestrator answers agent questions, reviews output, reassigns work, splits tasks. When you come back:

```
you: what happened?
orchestrator: while you were away (12 min):
  - auth-fix completed. PR-ready, 3 files changed.
  - page-builder finished AccountActivity, started ChatActivity.
    asked about ViewPager2 — I told it to skip it, use plain RecyclerView.
  - ui-redesign hit a snag with ItemTouchHelper. I suggested
    custom OnTouchListener instead. It's back on track.
```

## The thin CLI wrapper

The only non-Claude-Code code. Responsibilities:

1. **tmux session lifecycle** — create session, add/remove panes
2. **Keyboard routing** — track who "owns" each pane (you or orchestrator). When you attach, lock the pane from orchestrator writes. When you detach, unlock.
3. **Agent spawning** — `claude` process in a new pane + git worktree setup
4. **Keybindings** — attach, detach, dashboard navigation

Everything else — task scoping, prompt crafting, monitoring, splitting, answering questions, reviewing output — is the orchestrator Claude Code session doing its job.

## Non-goals (v1)

- Agents talking to each other (orchestrator mediates all cross-agent coordination)
- Web UI or desktop app (terminal-first)
- Non-Claude agents (Claude Code only)
- Persisting across machine restarts
- User interacting with sub-agents
