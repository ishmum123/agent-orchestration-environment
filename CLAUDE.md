# Orc

Rust TUI tool for running parallel Claude Code agents with an intelligent orchestrator.

## Build & Run

```bash
cargo build            # debug build
cargo build --release  # release build
cargo run              # run orc TUI
cargo run -- -p /path  # custom project directory
cargo run -- doctor    # check environment health
cargo test             # all tests (248 passing)
```

## Architecture

Single binary, single tokio runtime. All components run as tokio tasks communicating via channels.

### Core modules
- `src/main.rs` — CLI entry, async event loop (select! across crossterm + state broadcasts + orc output), all keybinds, attach/detach flow, doctor subcommand
- `src/app.rs` — AppState container: sessions, chat, tabs, scroll, modals, review state
- `src/session.rs` — SessionState enum (5 variants), BlockKind, SessionMode, pure `transition()` fn
- `src/state.rs` — StateManager (single writer), StateHandle (cloneable), 11 command types, 8 broadcast types
- `src/db.rs` — SQLite WAL mode, 6 tables, refinery migrations
- `src/orc.rs` — Orc brain process (Opus), stream-json parser, typed OrcEvent enum, usage tracking
- `src/mcp.rs` — JSON-RPC 2.0 server, 7 MCP tools, HTTP transport via axum (in-process)
- `src/policy.rs` — Permission policy from TOML, hard-deny constants, rule matching
- `src/hooks.rs` — Unix socket NDJSON listener, Python relay script, hooks config generation
- `src/tmux.rs` — tmux session lifecycle, attach with TMUX env clear for nesting
- `src/worktree.rs` — git worktree create/remove, cross-agent conflict detection
- `src/review.rs` — Unified diff parser, ReviewState with cursor nav, comments, approvals

### UI modules
- `src/ui/mod.rs` — Render dispatch: tabs → content → action bar → modal overlay
- `src/ui/tabs.rs` — Tab strip with state badges and mode indicators
- `src/ui/dashboard.rs` — Orc chat pane + task graph toggle
- `src/ui/worker.rs` — Worker tab: session info, PTY tail, permission decisions
- `src/ui/modals.rs` — Centered overlays: new task, ask_user, confirm, help
- `src/ui/review.rs` — Split-pane diff viewer: file tree + color-coded diff

### Test files
- `tests/phase0_validation.rs` — Integration tests: MCP, hooks, tmux lifecycle, attach/detach, full pipeline
- `tests/tui_integration.rs` — L3 TestBackend rendering tests for all views and states

## Conventions

- Async runtime: tokio (single-threaded by default, `#[tokio::main]`)
- State management: single-writer via StateManager, broadcast for UI updates
- Workers run in detached tmux sessions, not child processes
- MCP transport: HTTP (in-process axum server), not stdio
- Claude Code integration: `--output-format stream-json --verbose --input-format stream-json`
- tmux attach: must clear `TMUX` env var (`env_remove("TMUX")`) to allow nesting
- tmux detach: `bind -n C-q detach-client` (root-table binding, no prefix needed)
- Orc brain: `--dangerously-skip-permissions` so MCP tools aren't blocked
- Event loop: drain crossterm events synchronously before `select!` — async branches must not starve key input
- UI: ratatui with crossterm backend, TestBackend for tests
- All tmux test sessions use `tst-` prefix to avoid collision with `orc-` cleanup
- State badges: ◐ running, ? permission, ⏸ orc-decision, ! user-input, ◑ review, ✓ done, ✗ failed

## TUI End-to-End Testing (via tmux)

The TUI requires a real PTY — piping stdin fails with `Device not configured (os error 6)`. Use tmux to provide a PTY and drive the TUI non-interactively. **After any implementation work, run these checks before claiming done.**

### Primitives

```bash
# Launch — session name must NOT start with "orc-" (cleanup_orphaned kills those)
TMUX= tmux new-session -d -s test-runner -x 120 -y 40 \
  "TMUX= cargo run 2>/tmp/orc-stderr.log; echo EXIT_CODE=\$? > /tmp/orc-exit.log; sleep 30"

# Wait for startup (~6s for orc brain to connect)
sleep 6

# Capture current screen
TMUX= tmux capture-pane -t test-runner -p

# Send keys (-l for literal chars that tmux would interpret, e.g. ?)
TMUX= tmux send-keys -t test-runner -l '?'
TMUX= tmux send-keys -t test-runner Enter
TMUX= tmux send-keys -t test-runner Escape

# Check exit
cat /tmp/orc-exit.log   # should show EXIT_CODE=0

# Cleanup
TMUX= tmux kill-session -t test-runner 2>/dev/null
```

### Required checks after implementation

1. **Startup renders** — capture pane, verify tab bar (`● ORC`), chat pane (`sys: orc v2`), action bar
2. **Help modal** — send `?`, capture, verify `keybindings` modal; send `Escape`, verify dismissed
3. **New task modal** — send `n`, type text, verify buffer; send `Enter`, verify `you:` in chat
4. **Orc responds** — wait ~5s after submit, verify `orc:` message and cost in chat
5. **Worker spawn** — submit task like `spawn a worker to ...`, wait ~30s, verify:
   - Tab bar shows new worker tab with `◐` badge
   - `sys: session '...' spawned` in chat
   - Press `2` to switch to worker tab, verify session info renders
6. **Attach/detach** — on worker tab, press `Enter` to attach, verify tmux session visible; press `C-q` to detach, verify TUI resumes
7. **Graph toggle** — press `g`, verify split pane with task graph
8. **Confirm quit** — press `q` with running worker, verify confirm modal; `Escape` to cancel, then `q` + `y` to quit
9. **Clean exit** — check `EXIT_CODE=0`, no orphaned `orc-*` tmux sessions
10. **Doctor** — run `cargo run -- doctor`, verify all checks pass

### Gotchas
- `TMUX=` clears the env var — required for nested tmux
- `cargo run` spawns a real Claude session (costs tokens)
- stderr goes to `/tmp/orc-stderr.log` for debugging crashes
- Always `sleep 1` between send-keys and capture-pane for render cycle
- Worktrees at `../.orc-worktrees/` may need manual cleanup: `git worktree remove <path> --force`
