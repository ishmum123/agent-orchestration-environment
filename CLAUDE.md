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

- `src/main.rs` — CLI entry, terminal setup, event loop, keybind handlers, agent spawn/kill
- `src/app.rs` — App state machine, orc command parsing (SPAWN_AGENT/TELL_AGENT/KILL_AGENT), event draining
- `src/claude.rs` — ClaudeProcess: spawn `claude -p` with stream-json pipes, send/receive NDJSON
- `src/events.rs` — Typed serde structs for stream-json NDJSON events (system, assistant, result)
- `src/agent.rs` — Agent struct, AgentState enum, OutputLog for structured event history
- `src/orc.rs` — Orchestrator system prompt generation and spawning
- `src/worktree.rs` — git worktree creation/removal for agent isolation
- `src/ui.rs` — Ratatui dashboard, status view, agent detail, help/confirm overlays

## Conventions

- No async runtime — synchronous event loop with crossterm polling
- Shell out to `git` via `std::process::Command` for worktree management
- Claude Code instances are child processes with `--input-format stream-json --output-format stream-json`
- Communication via stdin/stdout NDJSON pipes (no tmux)
- Agent state derived from stream events, not heuristics
