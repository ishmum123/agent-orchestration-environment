# Orc

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
