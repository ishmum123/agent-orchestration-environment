# Orc v2 — Design & Implementation Plan

## What orc is

A terminal orchestrator that wraps Claude Code. You talk to orc; orc plans, delegates to background agents, monitors progress, and escalates when stuck. The user supervises from a ratatui TUI — chatting with orc, attaching into any worker as a full Claude Code session, reviewing diffs inline before merge.

Single binary. No API keys (Claude Code subscription only). No daemon.

## What changes from v1

v1: synchronous event loop, stream-json pipes, text command parsing (`[SPAWN_AGENT]`), no persistence, no attach, no review flow.

v2: tokio async, tmux-based workers, MCP-based orc tools (replacing text parsing), SQLite persistence, first-class attach, inline diff review, policy-driven permissions.

The rewrite keeps: worktree isolation, ratatui TUI, single crate structure, Claude Code as the only runtime.

---

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│                    orc binary (tokio)                     │
│                                                           │
│  ┌──────────┐  ┌────────────────────────────────────┐    │
│  │   TUI    │  │  MCP server (localhost SSE/stdio)   │    │
│  │ (ratatui)│  │                                     │    │
│  │          │  │  spawn_session(name, task, model?)   │    │
│  │          │  │  instruct_session(id, message)       │    │
│  │          │  │  kill_session(id)                    │    │
│  │          │  │  ask_user(question, context?)        │    │
│  │          │  │  list_sessions()                     │    │
│  │          │  │  mark_done(id, summary)              │    │
│  │          │  │  update_task_graph(graph)            │    │
│  └────┬─────┘  └──────────────▲──────────────────────┘    │
│       │                       │ tool calls                 │
│  ┌────▼─────────────┐  ┌─────┴──────────┐                │
│  │  state manager   │  │  orc brain     │                │
│  │  (SQLite + mem)  │  │  (Claude Code) │                │
│  │                  │  │  stream-json   │                │
│  │  sessions        │  │  --mcp-config  │                │
│  │  task graph      │  │  no file tools │                │
│  │  audit log       │  └────────────────┘                │
│  │  reviews         │                                     │
│  └────┬─────────────┘                                     │
│       │                                                   │
│  ┌────▼─────────┐   hook events (unix socket)             │
│  │ hook server  │◄───────────────────────────────┐       │
│  └──────────────┘                                │       │
│       │                                          │       │
│  ┌────▼────┐  ┌────────┐  ┌────────┐            │       │
│  │ tmux +  │  │ tmux + │  │ tmux + │            │       │
│  │ claude +│  │ claude │  │ claude │────────────┘       │
│  │ worktree│  │        │  │        │ (hooks fire here)   │
│  └─────────┘  └────────┘  └────────┘                     │
└──────────────────────────────────────────────────────────┘
```

### Why this shape

**Orc brain uses MCP tools, not text commands.** v1 parses `[SPAWN_AGENT name="..." task="..."]` from natural language via regex. Fragile — 56 tests just for command parsing. MCP gives orc proper tool calls: structured JSON in, structured JSON out. The MCP server runs in-process; orc calls `spawn_session` like any tool; the server executes it and returns a result. No parsing.

**Workers run in tmux, not pipes.** v1 communicates via stream-json stdin/stdout. Clean, but the user can never enter a worker session directly. tmux lets the user attach into any worker as a full Claude Code session — slash commands, @-mentions, file picker, everything. This is the primary UX differentiator.

**Hooks are the structured data channel, not capture-pane.** Claude Code fires hooks (PreToolUse, PostToolUse, Stop, etc.) with structured JSON payloads to a configurable endpoint. We receive these on a Unix socket. State is derived from hooks (authoritative). `tmux pipe-pane` logs terminal output to a file for display preview only — never used for state decisions.

**No Haiku watcher.** The spec proposes a persistent Haiku conversation monitoring all sessions. Too complex for the value: context rotation, calibration, false confidence. Replace with two-tier permissions: policy.toml rules (deterministic, sub-ms) → unmatched goes to orc (Opus, already running, has task context) → orc asks user if unsure.

**tokio, not sync polling.** v1 uses a 100ms poll loop. Works for pipes, but an MCP server, hook server, multiple tmux session readers, and SQLite writes all benefit from async. tokio is the Rust standard here.

**Single crate with modules, not a cargo workspace.** One developer. Module boundaries provide logical separation without build-system overhead. Split later if there's a reason.

---

## Key design decisions

### MCP server for orc tools

The orc brain is a Claude Code process — we cannot use the Anthropic API directly. Claude Code supports MCP servers natively. We run an MCP server in-process (SSE transport on localhost or stdio) and pass its config when spawning orc.

MCP tool definitions:

| Tool | Params | Returns | Notes |
|------|--------|---------|-------|
| `spawn_session` | name, task, model? | session_id, worktree_path, branch | model defaults to sonnet |
| `instruct_session` | session_id, message | delivered: bool | sends via tmux send-keys/load-buffer |
| `kill_session` | session_id | cleaned_up: bool | kills tmux session + removes worktree |
| `ask_user` | question, context? | response: str | **blocks** until user answers in TUI |
| `list_sessions` | — | array of session status objects | live state from state manager |
| `mark_done` | session_id, summary | ok: bool | orc declares a task node complete |
| `update_task_graph` | graph (JSON) | ok: bool | orc updates the plan |

`ask_user` is the key tool — its MCP response blocks until the user types an answer in the TUI modal. Orc's conversation pauses naturally because Claude Code is waiting for the tool result.

Orc can also call `list_sessions` mid-conversation to check current state before planning. No more injecting status updates into the conversation — orc queries when it needs to.

### tmux as worker container

Each worker session:
1. `git worktree add` creates isolated checkout (branch `orc/{name}`)
2. `tmux new-session -d -s orc-{name}` creates detached session
3. Generated `.tmux.conf` per worker: `status off`, no prefix, custom detach key (`C-q`)
4. `tmux send-keys` starts `claude -p` with appropriate flags inside the session
5. Claude Code hooks configured to fire to orc's Unix socket

Communication with workers:
- **Sending instructions:** `tmux load-buffer` + `tmux paste-buffer` (handles special chars). For simple messages, `tmux send-keys`.
- **Reading state:** Hook events (authoritative). `tmux pipe-pane -o {logfile}` for display preview.
- **User attach:** TUI suspends ratatui (leave alt-screen), runs `tmux attach -t orc-{name}`, resumes ratatui on detach (`C-q`). User sees full-screen Claude Code with no tmux chrome.

### Watch/Control modes

Orthogonal to session state. Every session is in exactly one mode:

**Watch (default):** orc monitors and instructs. User observes in TUI.

**Control:** user has taken over. For that session:
- Orc will not instruct it or replan around it (treats as paused)
- Permission auto-resolution pauses (events still log)
- User drives directly via tmux attach
- On exit: diff between mode-entry and mode-exit is captured. Orc gets a summary of what the user did.

Entered via `enter` (attach, implies control) or `c` (control without attaching — useful for manual worktree edits). TUI badge shows `[WATCH]` or `[CTRL]`.

### Permission system

Two tiers, no Haiku:

```
Hook fires PermissionRequest
  → orc binary reads payload
  → match against policy.toml
      ├─ allow      → respond, log
      ├─ deny       → respond, log
      ├─ hard_deny  → respond, log + alert (coded constants, overrides policy)
      └─ no match   → queue for orc
                      → orc decides (instruct/allow/deny) or asks user
```

**policy.toml** (repo root or `~/.config/orc/policy.toml`):

```toml
[allow]
read_in_repo = true
write_in_worktree = true
run_tests = true
run_lint = true

[escalate]
write_outside_worktree = true
network_calls = true
package_install = true

[deny]
modify_secrets = true
```

**Hard-deny constants** (coded, override everything):
- Force push to any remote
- Destructive ops on main/master (rm, reset --hard, push -f)
- Writes to system paths (/etc, /usr, ~/.ssh)

### Session states

5 states, not the spec's 9. Simpler to implement, test, and reason about.

```rust
enum SessionState {
    Running,
    Blocked { kind: BlockKind, reason: String },
    AwaitingReview { diff_hash: String },
    Done { summary: String },
    Failed { reason: String },
}

enum BlockKind {
    Permission,     // waiting for permission decision
    OrcDecision,    // orc needs to decide something
    UserInput,      // user needs to answer
}

enum SessionMode {
    Watch,
    Control,
}
```

### Review flow

Triggered when a worker's Claude Code `Stop` hook fires (agent says done). Session enters `AwaitingReview`.

**Review view:**
- Left pane: file tree with change counts
- Right pane: diff with line numbers
- `j/k` line nav, `J/K` hunk nav
- `c` on line: comment overlay
- `a` on hunk: approve marker
- `s` submit, `q` cancel (saves draft)

**Submitted payload:** structured JSON sent back to the worker via tmux. Worker resumes with the review as its next prompt.

```json
{
  "session_id": "...",
  "diff_hash": "...",
  "comments": [
    {"file": "src/auth.rs", "line": 47, "body": "..."},
    {"file": "src/auth.rs", "hunk": 3, "kind": "approve"}
  ],
  "overall": "approve" | "request_changes"
}
```

Comments are passed verbatim to the worker. Not pre-summarized, not rewritten.

### Persistence (SQLite)

`~/.config/orc/state.db`, single writer, WAL mode.

```sql
-- Core
sessions(id, name, prompt, worktree_path, branch, base_commit,
         tmux_session, state, state_data JSON, mode,
         created_at, updated_at, ended_at)

-- Append-only audit
state_transitions(id, session_id, from_state, to_state,
                  reason, triggered_by, at)

permission_decisions(id, session_id, request JSON, decision,
                     decided_by, reason, at)

-- Task planning
task_graphs(run_id, user_prompt, graph JSON,
            created_at, updated_at)
task_nodes(id, run_id, description, depends_on JSON, status,
           session_id, created_at, updated_at)

-- Reviews
reviews(id, session_id, diff_hash, comments JSON, overall,
        status, created_at, submitted_at)
```

Schema migrations via `refinery` from day one.

Ephemeral state (lost on crash, acceptable):
- PTY tail buffers (`VecDeque<String>` per session, last ~500 lines)
- Hook event queue
- TUI-local state (focused tab, scroll, modal, pending input)

### UI layout

```
┌─[ ORC ]────┬─ auth-fix ──────────┬─ edit-redesign ─────┬─ check-api ────┐
│ ●          │ ◐ running 14m WATCH │ ◑ review 3m         │ ✓ done 8m      │
└────────────┴─────────────────────┴─────────────────────┴────────────────┘
┌─ chat ───────────────────────────────┬─ task graph ─────────────────────┐
│ you: fix login re-prompting and      │ ┌── auth-fix ────┐              │
│      redesign edit/delete UI         │ │ ◐ running      │              │
│                                      │ └───────┬────────┘              │
│ orc: planned 3 sessions.            │         │                        │
│   spawning auth-fix, edit-redesign, │ ┌───────▼────────┐              │
│   check-api.                        │ │ edit-redesign   │              │
│                                      │ │ ◑ review       │              │
│ orc: check-api done — all healthy.  │ └────────────────┘              │
│                                      │                                 │
│ you: _                               │ ┌────────────────┐              │
│                                      │ │ check-api      │              │
│                                      │ │ ✓ done         │              │
│                                      │ └────────────────┘              │
└──────────────────────────────────────┴─────────────────────────────────┘
 n new  1-3 tab  g graph  enter attach  r review  k kill  ? help    q quit
```

**State badges:**

| State | Glyph | Color |
|-------|-------|-------|
| Running | `◐` | blue |
| Blocked (permission) | `?` | yellow |
| Blocked (user) | `!` | red blink |
| AwaitingReview | `◑` | magenta |
| Done | `✓` | green |
| Failed | `✗` | red |

### Concurrency model

```
                 ┌──────────────────────┐
                 │  State Manager       │ ← owns SQLite write handle
                 │  (tokio task)        │
                 └────────┬──────┬──────┘
                          │      │
             commands ────┘      └──── broadcasts
                          │      │
   ┌──────────┬───────────┴──┬───┴──────────┬──────────────┐
   │          │              │              │              │
┌──▼──┐  ┌───▼────┐  ┌──────▼─────┐  ┌────▼─────┐  ┌────▼──────┐
│ TUI │  │  MCP   │  │ hook       │  │ orc      │  │ pipe-pane │
│     │  │ server │  │ server     │  │ event    │  │ readers   │
│     │  │        │  │ (socket)   │  │ reader   │  │           │
└─────┘  └────────┘  └────────────┘  └──────────┘  └───────────┘
```

Single writer pattern. No two tasks race on session state because no two tasks write directly — all mutations go through the state manager's command channel.

TUI reads projections of authoritative state (updated via broadcast). UI-only state (focused tab, scroll, modals) is owned solely by TUI.

---

## Module layout

```
src/
├── main.rs              # binary entry, tokio runtime, CLI
├── state.rs             # state manager, command channel, broadcast
├── db.rs                # SQLite schema, migrations, queries
├── policy.rs            # policy.toml parsing, rule matching, hard-deny constants
├── mcp.rs               # MCP server (orc's tool definitions + handlers)
├── hooks.rs             # Unix socket hook receiver, event routing
├── tmux.rs              # tmux session lifecycle, send-keys, capture, attach/detach
├── worktree.rs          # git worktree create/remove (keep existing)
├── orc.rs               # orc Claude Code spawn, system prompt, MCP config
├── session.rs           # Session struct, SessionState, SessionMode
├── review.rs            # diff parsing, review state, payload construction
├── ui/
│   ├── mod.rs           # top-level render dispatch
│   ├── dashboard.rs     # orc tab: chat + task graph
│   ├── tabs.rs          # tab strip rendering + badges
│   ├── worker.rs        # worker tab: pty preview + status + decisions
│   ├── review.rs        # diff viewer: file tree + line nav + comments
│   └── modals.rs        # overlays: new task, ask user, confirm, help
└── tests/
    └── integration/
```

---

## Implementation plan

### Phase 0 — Validate assumptions (2-3 days)

Before committing to the architecture, prove two things:

**Validate MCP round-trip.** Write a minimal MCP server (one tool: `echo`). Spawn Claude Code in stream-json mode with `--mcp-config` pointing to it. Send a prompt that triggers the tool call. Verify: Claude Code calls the tool, server receives structured JSON, returns result, Claude Code continues.

If this fails: fall back to JSON-in-code-blocks protocol (orc emits ````orc\n{...}\n````, we parse). Worse than MCP, better than regex `[SPAWN_AGENT]`.

**Validate hooks as event channel.** Spawn Claude Code in a tmux session with hooks configured to write to a Unix socket. Give it a task that triggers file reads/writes. Verify: hook events arrive with structured payloads, timing is acceptable (<100ms), all tool uses generate events.

If hooks are too coarse (missing events we need): supplement with `tmux pipe-pane` parsing for progress display, keep hooks for state transitions only.

**Validate tmux attach/detach.** Suspend ratatui (leave alt-screen), exec `tmux attach`, verify clean terminal on detach, resume ratatui (re-enter alt-screen). Test on at least 2 terminal emulators.

### Phase 1 — Substrate (1 week)

Foundation: tmux, worktrees, hook server, SQLite, state manager.

**tmux.rs:**
- `create_session(name, working_dir)` — detached session with generated `.tmux.conf`
- `kill_session(name)` — cleanup
- `send_text(name, text)` — `load-buffer` + `paste-buffer` for reliability
- `start_pipe_pane(name, logfile)` — terminal output to file
- `read_pipe_log(logfile, last_n_lines)` — for TUI preview
- `attach(name)` — blocking, returns on detach
- Custom detach binding: `C-q`

**hooks.rs:**
- Unix socket listener (tokio)
- Parse Claude Code hook payloads (PreToolUse, PostToolUse, Stop, Notification)
- Route to state manager command channel
- Each hook payload includes the session name (set via environment variable or hook config)

**db.rs:**
- Schema creation + refinery migrations
- Insert/query for sessions, state_transitions, permission_decisions, task_graphs, reviews
- Single connection, WAL mode

**state.rs:**
- `StateManager` tokio task: owns DB write handle, receives commands via mpsc, broadcasts changes
- Commands: CreateSession, UpdateState, RecordPermission, UpdateTaskGraph, SaveReview, etc.
- Broadcasts: SessionChanged, TaskGraphChanged, NewPermissionDecision, etc.

**session.rs:**
- Session struct, SessionState enum (5 variants), SessionMode enum
- `transition(state, event) -> Result<SessionState>` pure function
- Unit tests for every valid transition

**worktree.rs:**
- Keep existing implementation, adapt to async (spawn_blocking for git commands)

**Exit criteria:** spawn a claude process in tmux in a worktree, receive hook events on the socket, persist session to SQLite, attach/detach cleanly.

### Phase 2 — Orc brain + MCP (1 week)

The orchestrator with proper tool calls.

**mcp.rs:**
- MCP server implementation (SSE or stdio transport)
- Tool definitions: spawn_session, instruct_session, kill_session, ask_user, list_sessions, mark_done, update_task_graph
- Each tool handler sends a command to state manager, awaits result, returns to Claude Code
- `ask_user` blocks its response until TUI signals the user's answer (oneshot channel)

**orc.rs:**
- Rewrite spawn_orc: generate MCP config, pass via `--mcp-config`
- System prompt: describe available tools, role, rules (no more command format docs)
- Orc process reader (stream-json): parse events for display in TUI chat pane
- Orc doesn't need `--tools ""` anymore — MCP tools are the only tools it sees (still disable file tools, LSP, etc.)

**policy.rs:**
- Parse `policy.toml` (serde + toml crate)
- `match_request(request) -> PolicyDecision` (Allow, Deny, HardDeny, Escalate)
- Hard-deny constants checked first, then policy rules
- Hot-reload on file change (notify crate or periodic stat)

**Integration:**
- Hook event for PermissionRequest → policy engine → auto-respond or queue for orc
- Orc receives permission escalation as context (injected into conversation or via an MCP tool like `pending_escalations()`)

**Exit criteria:** user types a task in TUI → orc calls `spawn_session` MCP tool → worker spawns in tmux with worktree → hook events flow → orc calls `list_sessions` to check status → orc calls `ask_user` and TUI shows modal.

### Phase 3 — TUI (1 week)

Full ratatui interface matching the design.

**ui/dashboard.rs:**
- Orc tab: chat history (scrollable) + task graph (toggle `g`)
- Task graph: render DAG from state manager projection

**ui/tabs.rs:**
- Tab strip: orc pinned left, workers as siblings
- State badges + mode badge + elapsed time
- Tab creation/removal synced with session lifecycle

**ui/worker.rs:**
- Worker tab: PTY preview (from pipe-pane log), session details, recent permission decisions
- State badge, model, worktree path, token usage (if available from hooks)

**ui/modals.rs:**
- New task input (textarea overlay)
- Ask user (question display + response textarea) — connected to `ask_user` MCP tool's oneshot channel
- Confirm kill/quit
- Help overlay

**Keybinds:**

| Key | Context | Action |
|-----|---------|--------|
| `n` | global | new task (modal) |
| `1-9` | global | jump to tab |
| `tab`/`shift-tab` | global | cycle tabs |
| `g` | orc tab | toggle task graph |
| `enter` | worker tab | attach (suspend TUI, tmux attach, control mode) |
| `c` | worker tab | control mode without attach |
| `r` | worker tab (AwaitingReview) | enter review view |
| `R` | worker tab (Failed) | restart session |
| `k` | worker tab | kill session (confirm) |
| `q` | global | quit (confirm if active sessions) |
| `?` | global | help |

**Attach/detach flow:**
1. User presses `enter` on a worker tab
2. TUI: set session mode to Control, disable raw mode, leave alt-screen
3. Exec `tmux attach -t orc-{name}`
4. User interacts with full Claude Code
5. User presses `C-q` (custom detach binding)
6. tmux detaches, control returns to orc binary
7. TUI: re-enter alt-screen, enable raw mode, resume rendering
8. Capture diff between control-entry and now, summarize for orc

**Exit criteria:** full TUI renders with tabs, task graph, worker preview, modals. Attach/detach works without terminal corruption. All keybinds functional.

### Phase 4 — Review flow (1 week)

**review.rs:**
- `compute_diff(worktree, base_commit)` — shell out to `git diff`, parse unified diff
- `ParsedDiff` struct: files → hunks → lines with +/-/context
- `ReviewState`: cursor position, draft comments, hunk approvals
- `ReviewPayload`: serialize comments + overall decision to JSON

**ui/review.rs:**
- File tree (left pane) with change counts and comment markers
- Diff view (right pane) with line numbers, syntax-aware coloring
- `j/k` line, `J/K` hunk, `gg/G` top/bottom
- `c` on line: comment textarea overlay, save attaches to line
- `a` on hunk: approve marker
- `s` submit: serialize payload, send to worker via tmux, transition session back to Running
- `q` cancel: save draft to SQLite for crash safety

**Review drafts persist on every keystroke** — if orc crashes mid-review, draft reloads from SQLite on restart.

**Exit criteria:** worker finishes → session enters AwaitingReview → user presses `r` → diff view renders → user comments on lines → submits → worker receives structured feedback and resumes.

### Phase 5 — Polish, dogfood, ship (1-2 weeks)

- **Restart crashed sessions** (`R` key): preserve prompt + worktree, spawn fresh Claude Code
- **Policy hot-reload**: detect file changes, re-parse
- **`orc doctor`**: check tmux, git, claude CLI, API connectivity, hook installation
- **Error recovery**: clean up orphaned tmux sessions on startup, handle worker crashes gracefully
- **Terminal compatibility**: test on iTerm2, kitty, alacritty, default Terminal.app
- **Cross-agent conflict detection**: orc tracks files touched per session (from PostToolUse hooks), warns on overlap
- **Orc context management**: monitor orc's own token usage, warn user when approaching limits
- **Dogfood on real work for a week, fix what hurts**

---

## Failure handling

- **Worker crash:** hook server stops receiving events → after timeout, state manager marks Failed. Orc notified, can replan or ask user. `R` key restarts.
- **Orc context full:** banner in TUI. User starts a new run. Don't try to be clever — the user needs to see this.
- **Network blip / 5xx:** Claude Code handles retries internally. If sustained (>5 min), workers may stall — orc banner, pause new task acceptance.
- **tmux session dies:** detected via `tmux has-session`. State manager marks Failed.
- **Hook server unreachable:** hooks block (Claude Code waits for response). If the socket is down, workers freeze — this is the fail-safe behavior we want (never auto-approve on infrastructure failure).
- **Orphaned worktrees:** `orc doctor` detects and offers cleanup. Startup checks for leftover `orc-*` tmux sessions.

---

## Testing strategy

Five layers. Weight is heavily on L1-L3 (fast, deterministic). L4-L5 are smoke tests, not comprehensive suites.

### L1 — Pure logic (sub-millisecond, run on every save)

No IO, no async, no LLM. These are the correctness backbone.

**State machine:**
```rust
#[test]
fn running_to_blocked_on_permission_request() {
    let s = SessionState::Running;
    let e = SessionEvent::PermissionRequested { request: mock_perm() };
    let s2 = transition(s, e).unwrap();
    assert!(matches!(s2, SessionState::Blocked { kind: BlockKind::Permission, .. }));
}

#[test]
fn cannot_transition_from_done() {
    let s = SessionState::Done { summary: "finished".into() };
    let e = SessionEvent::PermissionRequested { request: mock_perm() };
    assert!(transition(s, e).is_err());
}
```

Every valid transition has a test. Every invalid transition asserts error. Use `proptest` to fuzz random (state, event) pairs and verify no panics.

**Policy matching:**
```rust
#[test]
fn read_in_repo_auto_allowed() {
    let policy = Policy::from_str("[allow]\nread_in_repo = true");
    let req = PermRequest::read("src/main.rs");
    assert_eq!(policy.match_request(&req), PolicyDecision::Allow);
}

#[test]
fn hard_deny_overrides_policy_allow() {
    let policy = Policy::from_str("[allow]\nforce_push = true"); // user tries to allow it
    let req = PermRequest::force_push("origin", "main");
    assert_eq!(policy.match_request(&req), PolicyDecision::HardDeny);
}
```

**Diff parsing:**
```rust
#[test]
fn parse_unified_diff_multiple_hunks() {
    let raw = include_str!("../fixtures/multi_hunk.diff");
    let diff = ParsedDiff::parse(raw).unwrap();
    assert_eq!(diff.files.len(), 2);
    assert_eq!(diff.files[0].hunks.len(), 3);
    assert_eq!(diff.files[0].additions, 47);
}
```

**Task graph:**
```rust
#[test]
fn marking_node_done_unblocks_dependents() {
    let mut g = TaskGraph::new();
    let a = g.add_node("explore");
    let b = g.add_node_depends_on("implement", &[a]);
    assert!(!g.is_ready(b));
    g.mark_done(a, "found 3 handlers");
    assert!(g.is_ready(b));
}
```

**Review payload serialization:** round-trip through serde, verify structure matches what workers expect.

### L2 — Component integration (cargo test, seconds)

Real IO against real local resources. Each test creates its own tempdir, tears it down after.

**SQLite:**
```rust
#[tokio::test]
async fn session_persists_across_state_transitions() {
    let dir = tempdir().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    let id = db.create_session("test", "do stuff", "/tmp/wt").unwrap();
    db.record_transition(id, "Running", "Done", "completed").unwrap();
    let session = db.get_session(id).unwrap();
    assert_eq!(session.state, "Done");
    let transitions = db.get_transitions(id).unwrap();
    assert_eq!(transitions.len(), 1);
}
```

Verify: schema migrations apply cleanly, insert/query round-trips, WAL mode works, concurrent reads don't block.

**Hook server:**
```rust
#[tokio::test]
async fn hook_event_round_trip() {
    let sock = tempdir().unwrap().path().join("hooks.sock");
    let (tx, mut rx) = mpsc::channel(32);
    let server = HookServer::bind(&sock, tx).await.unwrap();
    tokio::spawn(server.run());

    // Simulate Claude Code hook firing
    let client = UnixStream::connect(&sock).await.unwrap();
    send_hook_payload(&client, json!({
        "session_id": "test-1",
        "hook": "PostToolUse",
        "tool": "Read",
        "file": "src/main.rs"
    })).await;

    let event = rx.recv().await.unwrap();
    assert_eq!(event.session_id, "test-1");
    assert!(matches!(event.kind, HookKind::PostToolUse { .. }));
}
```

**Worktree:**
```rust
#[test]
fn create_and_remove_worktree() {
    let repo = create_test_repo(); // git init + initial commit in tempdir
    let wt = create_worktree(&repo, "test-agent").unwrap();
    assert!(wt.path.exists());
    assert!(wt.path.join(".git").exists());
    remove_worktree(&repo, "test-agent").unwrap();
    assert!(!wt.path.exists());
}
```

**MCP server:**
```rust
#[tokio::test]
async fn mcp_spawn_session_tool_call() {
    let (state_tx, state_rx) = mpsc::channel(32);
    let server = McpServer::new(state_tx);
    let result = server.handle_tool_call("spawn_session", json!({
        "name": "test-worker",
        "task": "read src/main.rs",
        "model": "sonnet"
    })).await;
    assert!(result["session_id"].is_string());

    // Verify state manager received the command
    let cmd = state_rx.try_recv().unwrap();
    assert!(matches!(cmd, StateCommand::CreateSession { .. }));
}
```

**Policy hot-reload:**
```rust
#[tokio::test]
async fn policy_reloads_on_file_change() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("policy.toml");
    fs::write(&path, "[deny]\nnetwork_calls = true").unwrap();
    let policy = PolicyEngine::watch(&path).await.unwrap();

    assert_eq!(policy.match_request(&net_request()), PolicyDecision::Deny);

    fs::write(&path, "[allow]\nnetwork_calls = true").unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(policy.match_request(&net_request()), PolicyDecision::Allow);
}
```

### L3 — TUI snapshots (cargo test, sub-second)

ratatui `TestBackend` renders to an in-memory buffer. `insta` snapshots checked into `tests/golden/`. `cargo insta review` to inspect diffs.

```rust
#[test]
fn dashboard_with_running_and_done_sessions() {
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    let app = test_app_state(vec![
        ("auth-fix", SessionState::Running, 840),      // 14 min
        ("edit-redesign", SessionState::AwaitingReview { .. }, 180),
        ("check-api", SessionState::Done { .. }, 480),
    ]);
    terminal.draw(|f| render(f, &app)).unwrap();
    insta::assert_snapshot!("dashboard_mixed_states", terminal.backend().to_string());
}

#[test]
fn review_view_with_comments() {
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    let app = test_app_with_review(
        include_str!("../fixtures/sample.diff"),
        vec![DraftComment { file: "src/auth.rs", line: 18, body: "make this required?" }],
    );
    terminal.draw(|f| render(f, &app)).unwrap();
    insta::assert_snapshot!("review_with_comment", terminal.backend().to_string());
}

#[test]
fn ask_user_modal_overlay() {
    // ...renders modal centered over dashboard content
    insta::assert_snapshot!("ask_user_modal", terminal.backend().to_string());
}
```

**Required snapshots** (one per meaningful UI state):
- Dashboard: no agents, mixed states, all done, orc thinking
- Worker tab: running, blocked (permission), blocked (user input), failed
- Tab strip: with mode badges, many tabs (overflow/truncation)
- Review: file tree + diff, with/without comments, hunk approval markers
- Modals: new task, ask user, confirm kill, confirm quit, help
- Edge cases: narrow terminal (80x24), very long session names, many sessions

### L4 — tmux integration (shell script, seconds)

Drive the real binary via tmux. These are smoke tests — verify the happy path works end-to-end, not edge cases.

```bash
#!/bin/bash
# tests/integration.sh
set -euo pipefail

SESSION="orc-test-$$"
BINARY="./target/release/orc"
REPO=$(mktemp -d)

# Setup test repo
git -C "$REPO" init && git -C "$REPO" commit --allow-empty -m "init"

# Launch orc in tmux
tmux new-session -d -s "$SESSION" -x 200 -y 50 "$BINARY -p $REPO"
sleep 3

# Verify orc started
SCREEN=$(tmux capture-pane -t "$SESSION" -p)
assert_contains "$SCREEN" "ready"

# Type a task
tmux send-keys -t "$SESSION" "n"
sleep 0.5
tmux send-keys -t "$SESSION" "list files in this repo" Enter
sleep 10

# Verify agent spawned (check for state badge)
SCREEN=$(tmux capture-pane -t "$SESSION" -p)
assert_contains "$SCREEN" "◐"  # running badge

# Wait for completion
wait_for_content "$SESSION" "✓" 60  # timeout 60s

# Verify no raw command tags leak into display
SCREEN=$(tmux capture-pane -t "$SESSION" -p)
assert_not_contains "$SCREEN" "spawn_session"
assert_not_contains "$SCREEN" "MCP"

# Verify worktree created
assert_dir_exists "$REPO/../.orc-worktrees/"

# Kill agent via TUI
tmux send-keys -t "$SESSION" "1"   # select worker tab
sleep 0.5
tmux send-keys -t "$SESSION" "k"   # kill
sleep 0.5
tmux send-keys -t "$SESSION" "y"   # confirm
sleep 2

# Verify worktree cleaned up
assert_dir_empty "$REPO/../.orc-worktrees/"

# Quit
tmux send-keys -t "$SESSION" "q"
sleep 0.5
tmux send-keys -t "$SESSION" "y"
sleep 2

# Verify tmux session gone
! tmux has-session -t "$SESSION" 2>/dev/null

# Verify no orphaned orc-* tmux sessions
ORPHANS=$(tmux list-sessions -F '#{session_name}' 2>/dev/null | grep '^orc-' || true)
[ -z "$ORPHANS" ] || fail "orphaned sessions: $ORPHANS"

rm -rf "$REPO"
echo "PASS"
```

**What this covers:** orc startup, agent spawn via MCP, completion detection, display cleanliness, worktree lifecycle, quit cleanup.

**What this doesn't cover:** attach/detach (requires interactive terminal), review flow (requires agent producing real changes), permission escalation (requires specific hook triggers). These are tested manually during dogfooding.

### L5 — Fixture-driven scenarios (manual + CI nightly)

Pre-built scenarios for deterministic testing without real LLM calls. Each fixture includes a fake-claude shim that replays canned responses.

**fixtures/ layout:**
```
fixtures/
├── fake-claude/          # shim binary, reads FIXTURE env, replays responses
├── demos/
│   ├── spawn-and-complete/
│   │   ├── orc-responses.ndjson    # canned orc MCP tool calls
│   │   └── worker-hooks.ndjson     # canned hook events
│   ├── permission-escalation/
│   │   ├── orc-responses.ndjson
│   │   ├── worker-hooks.ndjson
│   │   └── policy.toml
│   ├── review-flow/
│   │   ├── orc-responses.ndjson
│   │   ├── worker-hooks.ndjson
│   │   └── sample.diff
│   ├── crash-and-restart/
│   ├── ask-user-flow/
│   └── multi-agent-parallel/
└── diffs/
    ├── simple_add.diff
    ├── multi_file.diff
    ├── rename.diff
    └── binary_file.diff
```

**`orc --demo-mode <name>`**: loads fixture, replaces claude binary with fake-claude via PATH, replays scenario deterministically.

**fake-claude shim:**
- Reads `ORC_FIXTURE` env var to find response file
- Replays NDJSON lines from the fixture with configurable delays
- For MCP tool calls: returns pre-recorded tool results
- For hooks: fires pre-recorded events to the hook socket
- Exits with appropriate status on completion

This lets you test the full stack — TUI, state manager, MCP handling, hook processing, review flow — without any API calls. CI runs these nightly. Developers run them via `cargo run -- --demo-mode <name>` to visually verify behavior.

### Dev observability

Tools for development iteration, especially useful when building with AI assistance (the AI can't see a live TUI):

**`orc dump-state`**: Pretty-print current SQLite state as JSON. Sessions, task graph, recent transitions, last 50 permission decisions. Run alongside to inspect state without the TUI.

**`orc tail-events --json`**: Stream every state change, hook event, MCP tool call as NDJSON to stdout. Pipe to `jq` for filtered views. Useful for debugging event flow.

**tmux capture for AI-assisted dev**: During development, capture the TUI state via `tmux capture-pane -t orc-dev -p > /tmp/screen.txt`. AI reads the file to see what the TUI looks like. No screenshots needed.

**Structured logging**: All components log to stderr with structured format (session_id, component, event). `RUST_LOG=orc=debug` for verbose output during development. Logs rotate to `~/.config/orc/logs/`.

### Test coverage targets

| Layer | Scope | Count (est.) | Runtime |
|-------|-------|-------------|---------|
| L1 | State, policy, diff, graph | 80-120 tests | <1s |
| L2 | DB, hooks, MCP, worktree | 30-50 tests | <5s |
| L3 | TUI snapshots | 20-30 snapshots | <2s |
| L4 | tmux smoke | 1 script, ~5 assertions | <90s |
| L5 | Fixture scenarios | 6-8 demos | manual/nightly |

L1-L3 run on every `cargo test`. L4 runs in CI. L5 runs nightly or before release.

### Phase-specific test milestones

**Phase 1 exit:** L1 tests for state machine + policy. L2 tests for SQLite + hooks + worktree. At least one L4 smoke assertion (orc starts in tmux).

**Phase 2 exit:** L2 tests for MCP round-trip. L1 tests for policy matching. Fixture for `spawn-and-complete` scenario works with fake-claude.

**Phase 3 exit:** L3 snapshots for every UI state. L4 smoke test covers spawn → complete → cleanup.

**Phase 4 exit:** L1 tests for diff parsing. L3 snapshots for review view. Fixture for `review-flow` scenario.

**Phase 5 exit:** All L4 assertions pass. All fixtures run clean. `orc doctor` reports healthy. Manual attach/detach tested on 2+ terminal emulators.

---

## Hard cuts (post-v2)

- Haiku watcher / auto-permission adjudication by LLM
- Watcher self-rotation / context handoff
- Multi-runtime (Codex, Aider, etc.) — trait designed only
- Daemon mode / detached operation
- Windows support (tmux dependency)
- Web dashboard
- Merge queue (manual git merge after review)
- Per-session context budgets
- Session merging (overlapping code detection → pause one)
- Speculative parallel exploration (same task, two approaches)
- Cross-session evidence sharing
- Memory across orc runs

---

## Dependencies (expected)

| Crate | Purpose |
|-------|---------|
| `tokio` | async runtime |
| `ratatui` + `crossterm` | TUI (keep from v1) |
| `rusqlite` | SQLite |
| `refinery` | schema migrations |
| `serde` + `serde_json` + `toml` | serialization |
| `clap` | CLI (keep from v1) |
| `anyhow` | errors (keep from v1) |
| `notify` | file watcher (policy hot-reload) |
| `insta` | snapshot testing |

MCP server: likely hand-rolled over hyper/axum (SSE transport) or raw stdio. Evaluate existing `mcp-server` crates before writing from scratch.

---

## Open questions (answer before the relevant phase)

1. **MCP transport: SSE vs stdio?** SSE (localhost HTTP) is more standard but requires a port. stdio avoids networking but means the MCP server is a subprocess spawned by Claude Code, needing IPC back to the main process. Test both in Phase 0.

2. **How does `ask_user` unblock?** The MCP tool handler blocks on a oneshot channel. TUI writes the user's response to the channel. But if the user never answers (closes modal), we need a timeout or cancellation path. Design before Phase 2.

3. **Hook identification.** When multiple workers fire hooks to the same socket, how does the hook server know which session a hook belongs to? Options: (a) set `ORC_SESSION_ID` env var per worker, include in hook payload; (b) one socket per worker; (c) encode session ID in hook config path. Decide in Phase 1.

4. **Second-pass review.** If a worker gets review feedback, fixes things, and says done again — is the second review a diff against base, or incremental against the first review? Decide before Phase 4.

5. **Detach key conflict.** `C-q` is XON/XOFF flow control in some terminals. Test and pick an alternative if needed. Decide in Phase 1.
