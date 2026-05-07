# Orchestr8

A terminal tool that runs parallel Claude Code agents with an intelligent orchestrator (orc). You talk to agents when you want. The orc handles them when you don't.

## The gap

**Agent Teams:** smart orchestrator, but you can't naturally talk to individual agents.
**Claude Squad:** great direct-talk UX, but no intelligent coordination — you do everything.
**Orchestr8:** both. Smart lead + direct talk + autonomous coordination.

## Architecture

```
tmux session "orchestr8"
├── pane 0: orc (Claude Code)
│   ├── reads agent panes via capture-pane
│   ├── writes to agent panes via send-keys
│   ├── holds the plan, not the transcripts
│   └── updates CLAUDE.md with project learnings
├── pane 1: agent "auth-fix" (Claude Code)
│   └── own git worktree, own context
├── pane 2: agent "page-builder" (Claude Code)
│   └── own git worktree, own context
└── pane 3: agent "ui-redesign" (Claude Code)
    └── own git worktree, own context
```

The orc talks to agents by typing into their panes (`tmux send-keys`) and reads their output (`tmux capture-pane`). Agents can't tell whether you or the orc is typing — it's the same terminal.

All intelligence is Claude Code. The only non-Claude-Code piece is a thin CLI that manages tmux lifecycle, keyboard routing, and temporary panes.

## Dashboard

```
orchestr8 ─ 3 agents ─ 2 working ─ 1 waiting

▸ auth-fix       [working]   session token impl         ░░░░░▓▓
  page-builder   [waiting]   "RecyclerView or ViewPager2?"
  ui-redesign    [working]   swipe-to-reveal handler    ░░░▓▓▓▓

──────────────────────────────────────────────────────────────
[a]ttach [t]ell [e]dit [d]iff [s]tatus [n]ew [q]uit

last: orc answered page-builder re: AppCompatActivity (2m ago)
next: auth-fix approaching 40k context
```

- `j`/`k` or arrows to select agent (marked with `▸`)
- All keybinds act on the selected agent
- Context bar (`░░░▓▓`) shows token usage at a glance
- Bottom status: last orc action + next anticipated event

### Agent states

- **working** — agent is actively producing output
- **waiting for you** — agent asked something the orc deferred to you
- **idle** — agent finished its current task
- **done** — review passed, work is complete
- **stuck** — agent errored or looped, orc flagged it

## Keybinds and flow

All transitions return to dashboard. Temporary panes auto-close when the tool exits.

```
Dashboard ──[a]──→ Agent pane (full tmux pane switch)
    ↑                   │
    └───[Esc/detach]────┘

Dashboard ──[d]──→ lazygit (temp pane, agent's worktree vs main)
    ↑                   │
    └───[q in lazygit]──┘

Dashboard ──[e]──→ $EDITOR (temp pane, changed files at diff hunks)
    ↑                   │
    └───[close editor]──┘

Dashboard ──[t]──→ inline prompt (type message, Enter sends via orc)
Dashboard ──[s]──→ layered summary view (drill down with Enter, back with Esc)
Dashboard ──[n]──→ inline prompt (describe task, orc scopes + spawns)
```

### [d] — diff view

Opens lazygit in a temporary tmux pane, scoped to the selected agent's worktree:

```bash
lazygit --work-tree=/path/to/agent/worktree \
        --git-dir=/path/to/agent/worktree/.git \
        --use-config-file="$HOME/.orchestr8/lazygit.yml"
```

Orchestr8 generates a lazygit config that strips noise for this workflow:

```yaml
# ~/.orchestr8/lazygit.yml
gui:
  showBottomLine: false
  showCommandLog: false
  theme:
    activeBorderColor: ["green", "bold"]
git:
  paging:
    colorArg: always
    pager: delta --dark --paging=never
```

User sees only what this agent changed vs main. Not the full repo history.

### [e] — editor view

Opens `$EDITOR` in a temporary tmux pane with the agent's changed files, cursor positioned at diff hunks:

```bash
changed_files=$(cd $AGENT_WORKTREE && git diff --name-only HEAD)
# for nvim: generate quickfix list from diff hunks
git diff --unified=0 | parse_hunks > /tmp/orchestr8-qf.txt
nvim -q /tmp/orchestr8-qf.txt
```

User lands directly at the lines the agent changed, not file tops.

## Three ways to talk to an agent

### 1. Direct — attach to the agent's pane

```
[a] on dashboard
# tmux switches you to the agent's pane
# you type directly into its Claude Code session
# orc stops sending to this pane (locked)
Esc or detach
# back to dashboard, orc can resume
```

The agent sees a human typing. That's all it ever sees.

### 2. Through orc — relay with context

```
[t] on dashboard
> try session tokens instead of re-validating the ID token every call
```

The orc doesn't just parrot this. It adds context:
- "The server-side is in Code.gs, the token validation is on line 24-25"
- "The user wants you to issue your own session token after first verification"
- "This relates to the 50-minute re-prompt bug"

Then types the full enriched message into the agent's pane.

### 3. Orc independent — no input from you needed

Agent-2 asks: "Should AccountActivity extend AppCompatActivity or ComponentActivity?"

You're not there. The orc reads this from the pane, knows the project uses AppCompatActivity everywhere, types the answer. You never see the question unless you check logs.

## Spawning agents

You describe what you want at a high level:

```
[n] on dashboard
> the login keeps re-prompting every 50 minutes. the account/chat
  dialogs need to be full pages. the edit/delete UI needs a redesign.
```

The orc:
1. Explores the codebase (or uses context you've given it)
2. Breaks the work into scoped tasks — by problem seams, not file count
3. Crafts a focused prompt for each agent with file paths, constraints, scope boundaries
4. Spawns each agent in its own tmux pane + git worktree
5. Types the prompt into each pane

Agents start smart, not cold.

## Agent configuration

The orc generates a config per agent at spawn time:

```yaml
agents:
  auth-fix:
    scope: [src/auth/**, src/session/**]
    forbidden: [src/ui/**, *.config.*]
    max_context: 80000
    model: opus  # orc decides based on task complexity
  page-builder:
    scope: [src/ui/account/**, src/ui/chat/**]
    max_context: 50000
    model: sonnet
```

- **max_context** — upper limit, configurable globally and per-agent. The orc sets dynamic boundaries within this limit based on task progress.
- **scope/forbidden** — included in the agent's initial prompt as instructions. Not enforced by file permissions (Claude Code doesn't support per-session permissions), but caught in monitoring if violated.
- **model** — orc decides at spawn time. Opus for complex/architectural work, Sonnet for straightforward tasks, Haiku for trivial lookups.

## Context management

The orc manages agent context actively:

- **Scoped initial prompts** — agents get what they need, not the whole codebase
- **Monitoring** — orc reads agent panes every ~30 seconds to gauge progress and catch problems
- **Compaction** — orc tells the agent to /compact when approaching its context cap
- **Smart splitting** — when an agent runs out of room:

```
orc detects agent-2 at ~45k tokens (cap is 50k)
orc reads agent-2's pane to understand:
  - what it accomplished (AccountActivity done, ChatActivity half-done)
  - what decisions it made (chose Activities over Fragments, uses existing theme)
  - what remains (ChatActivity layout, bubble adapters, task picker)

orc spawns agent-4 "chat-builder" with:
  - key decisions from agent-2 (not the transcript)
  - remaining work, scoped precisely
  - links to what agent-2 already created

agent-2 is told to wrap up and finalize its completed work
```

This is not "copy the prompt and restart." The orc understands what's done and what's left, then designs a continuation around the actual remaining problem.

## Monitoring and safety

The orc reads agent panes every ~30 seconds. This serves two purposes:

**Progress tracking** — is the agent making progress or spinning? Update dashboard status and context bar.

**Safety** — catch agents going off-rail:
- Editing files outside their declared scope
- Looping on the same error repeatedly
- Attempting destructive operations
- Drifting from the assigned task

When the orc detects a problem:
1. **Redirect** — tell the agent to course-correct ("you're editing the wrong file, focus on src/auth/")
2. **Pause** — if redirect fails or the situation is dangerous, stop the agent and escalate to the user with `[waiting for you]` status

The orc's context isn't bloated by monitoring — it reads recent pane output (last ~50 lines), not full history. If deeper analysis is needed, orc spawns a short-lived Haiku summarization to digest the agent's full output and report back.

## Review pass

When an agent signals "done," the orc runs a review before marking it complete.

**Two-tier review:**

**Fast check (orc does this itself):**
- Did the agent touch the right files?
- Does the diff look proportional to the task?
- Did the agent say it's done and summarize what it did?

If fast check passes and the task is simple (<200 lines changed, no architectural decisions), mark as **done**.

**Deep review (separate short-lived reviewer agent):**
- Spawned for: large diffs, architectural decisions, tasks the user flagged as important
- Gets: the original task prompt + the diff + relevant project context
- Returns: verdict (pass/fail) + issues found
- Orc reads the verdict, tells the original agent to fix issues or marks as done

The reviewer agent is disposable — spawned on demand, killed after verdict. It doesn't persist.

## Merging

Agents work in separate git worktrees. When it's time to integrate:

1. Orc checks for file overlap between completed agents
2. If no overlap: fast-forward merge, no conflict possible
3. If overlap detected: orc spawns a short-lived merge agent that gets both diffs + the plan context, produces a merged result
4. If the merge agent can't resolve confidently: escalates to user

The user can also manually merge via `[d]` (lazygit on any worktree) or by attaching to the main repo.

## Layered summaries

The dashboard always shows one-line status per agent. When the user asks "what happened?" or returns from away mode, they get layered depth:

**Layer 0 — headline** (always on dashboard):
```
3 agents ─ 2 working ─ 1 waiting
```

**Layer 1 — per-agent summary** (press `[s]` or after away):
```
auth-fix: replaced ID token revalidation with session tokens.
  3 files changed. PR-ready.
page-builder: AccountActivity done, ChatActivity in progress.
  chose Activities over Fragments. skipped ViewPager2.
ui-redesign: swipe-to-reveal implemented, hit snag with ItemTouchHelper,
  switched to custom OnTouchListener.
```

**Layer 2 — full transcript** (press Enter on an agent in status view, or `status agent-1 --full`):
The agent's actual conversation, decisions, errors, recovery. Raw pane history.

**Layer 3 — diff** (press `[d]` on selected agent):
lazygit opens scoped to that agent's worktree. See exactly what changed, line by line.

**Layer 4 — editor** (press `[e]` on selected agent):
$EDITOR opens at the diff hunks. Read, annotate, modify the agent's output directly.

Summaries are generated at task boundaries (agent signals done, context split, user asks). Not continuously.

## Checkpoints

When the user says `checkpoint` (or orc auto-checkpoints after a major milestone):

1. Each agent's worktree is committed (if uncommitted work exists)
2. Orc writes `checkpoint.md` in the root repo:
   - Task plan at time of checkpoint
   - Per-agent status and key decisions
   - What orc decided autonomously (and why)
3. Everything committed to a checkpoint branch

This is git — no custom VCS. The metadata layer captures orc reasoning that would otherwise be lost when the tmux session ends.

Orc also updates CLAUDE.md with durable project learnings discovered during the session (architecture patterns, conventions, things that failed).

Restoring from checkpoint is v2. For v1, checkpoints are write-only records.

## Sub-agents

Agents can spawn their own sub-agents for internal parallelization. Max 2 layers deep:

```
Orc (layer 0)
└── Agent (layer 1) — visible on dashboard, you can talk to it
    └── Sub-agent (layer 2) — invisible to you, agent manages it
```

Sub-agents never appear on the dashboard. The agent that spawned them is responsible for their output.

## User modes

**Directing** — attached to an agent (`[a]`), typing directly. Orc doesn't interfere with that pane.

**Supervising** — on dashboard. You see status, review diffs, relay messages. Orc handles routine decisions.

**Away** — you walked away. Orc answers agent questions, reviews output, reassigns work, splits tasks. When you come back:

```
[s] or "what happened?"
orc: while you were away (12 min):
  - auth-fix completed. review passed. 3 files changed.
  - page-builder finished AccountActivity, started ChatActivity.
    asked about ViewPager2 — I told it to skip it, plain RecyclerView.
  - ui-redesign hit snag with ItemTouchHelper. I suggested
    custom OnTouchListener. back on track.
```

## The thin CLI wrapper

The only non-Claude-Code code. Responsibilities:

1. **tmux session lifecycle** — create session, add/remove panes, manage temporary panes
2. **Keyboard routing** — track who "owns" each pane (you or orc). Lock on attach, unlock on detach.
3. **Agent spawning** — `claude` process in a new pane + git worktree setup
4. **Keybinds** — as described in [Keybinds and flow](#keybinds-and-flow)
5. **lazygit config** — generate `~/.orchestr8/lazygit.yml` at session start

Everything else — task scoping, prompt crafting, monitoring, splitting, reviewing, answering questions — is Claude Code doing its job.

## Non-goals (v1)

- Agents talking to each other (orc mediates all cross-agent coordination)
- Web UI or desktop app (terminal-first, TUI only)
- Non-Claude agents (Claude Code only)
- Persisting across machine restarts (checkpoints are write-only records)
- User interacting with sub-agents
- Restoring orc state from checkpoints (v2)
- Custom diff viewer (lazygit + $EDITOR are sufficient)
- Per-session Claude Code file permissions (not supported; scope enforced via prompts + monitoring)
