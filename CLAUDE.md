# Orc

Rust TUI tool for running parallel Claude conversations with an orchestrator.

**Architecture and UX: see [SPEC.md](./SPEC.md). It is the source of truth.** Don't guess the design from code — read the spec first, then read code.

## Build & Run

```bash
cargo build            # debug build
cargo build --release  # release build
cargo run              # run orc TUI
cargo run -- -p /path  # run on a specific project
cargo run -- doctor    # environment health check
cargo test             # run all tests
cargo install --path . --force   # install global `orc`
```

## Verifying changes — REAL testing, not just unit tests

**Unit tests are necessary but not sufficient.** They prove the code you wrote does what you wrote it for. They do not prove orc works as a real interactive program. Before claiming any TUI/UX work is done, drive the live binary in a real terminal harness and confirm what the user would see.

The TUI requires a real PTY — `cargo run | …` fails with `Device not configured (os error 6)`. tmux is the harness: it gives orc a PTY, and `tmux send-keys` / `tmux capture-pane` let you drive it non-interactively. tmux here is *test infrastructure for orc*, not part of orc's architecture (workers are stream-json children, not tmux — see SPEC).

### Harness primitives

```bash
# Launch orc inside a tmux session. Session name must NOT start with `orc-`
# (orc's own startup sweep kills those).
TMUX= tmux new-session -d -s test-runner -x 140 -y 50 \
  "TMUX= cargo run 2>/tmp/orc-stderr.log; echo EXIT=\$? > /tmp/orc-exit.log; sleep 90"

# Wait for startup (orc brain takes a few seconds to connect).
sleep 8

# Capture what orc is showing.
TMUX= tmux capture-pane -t test-runner -p

# Drive keystrokes. Use -l for literal characters tmux would otherwise interpret.
TMUX= tmux send-keys -t test-runner -l '?'
TMUX= tmux send-keys -t test-runner Enter
TMUX= tmux send-keys -t test-runner Escape

# Inspect exit + crash logs.
cat /tmp/orc-exit.log         # expect EXIT=0
tail /tmp/orc-stderr.log      # for crash output

# Tear down.
TMUX= tmux kill-session -t test-runner 2>/dev/null
```

### After any TUI/UX/orchestration change, you MUST live-verify

Drive the binary through whichever flows your change touches. The minimum bar:

1. **Startup renders cleanly** — capture pane, confirm tab strip, orc tab content, action bar.
2. **The flow you changed actually works end-to-end** — type the keystrokes, capture the screen, read the output. Don't infer behavior; observe it.
3. **Clean exit** — `q` (and `y` if a confirm modal appears) → `EXIT=0`, no leftover background processes (`pgrep -lf orc`, `pgrep -lf claude`).
4. **`cargo run -- doctor` passes** with no orphans flagged.
5. **State is sane** — `sqlite3 ~/.config/orc/state.db "select name, state from sessions"` shows what you'd expect after the run.

If the change touches workers, drive a real worker spawn. That costs tokens. It also catches the bugs unit tests can't see — paste/Enter races, environment leaks, layout breakage, focus issues, modal stacking, real claude output shape.

### Useful inspection while running

```bash
# Live state in DB (orc holds an open handle; reads are safe).
sqlite3 ~/.config/orc/state.db "select name, state from sessions order by created_at desc;"

# Worker logs (if any are still being written).
ls /var/folders/*/T/orc-*.log 2>/dev/null
ls /tmp/orc-*.log 2>/dev/null

# Worktrees in flight.
git worktree list

# Anything orc-related still alive.
pgrep -lf orc
pgrep -lf claude
ps aux | grep -E "orc|claude" | grep -v grep
```

### Cleanup after a botched run

```bash
# Kill anything left over.
pkill -f "^orc$"; pkill -f "claude -p"
TMUX= tmux ls | grep '^orc-' | cut -d: -f1 | xargs -I{} tmux kill-session -t {}

# Wipe DB if you want a clean slate.
rm -f ~/.config/orc/state.db ~/.config/orc/state.db-shm ~/.config/orc/state.db-wal

# Prune orphaned worktrees.
git worktree prune
ls ../.orc-worktrees/ 2>/dev/null && rm -rf ../.orc-worktrees/*

# Old branches.
git branch | grep '^  orc/' | xargs git branch -D
```

## Conventions

- Async runtime: tokio.
- Single-writer state via `StateManager`; UI reads broadcasts.
- ratatui + crossterm for the TUI.
- All test-only tmux sessions use a `tst-` prefix to avoid collision with orc's startup cleanup.

Everything else (module map, file responsibilities, badge glyphs, key list) — read SPEC.md, then read the code. Don't take stale notes from this file.
