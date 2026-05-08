# Orc v2 — Status

Branch: `v2-rewrite` | Tests: 14 passing | ~10,900 lines across 21 files

## Bugs fixed (2026-05-08)

1. **Event loop key starvation** (`main.rs`) — `tokio::select!` with `poll_crossterm()` (50ms) vs `read_events()` (10ms) meant keys never processed. Fix: drain crossterm synchronously before `select!`, use 16ms frame tick.
2. **Workers invisible in UI** (`main.rs:593`, `state.rs:92`) — `SessionCreated` broadcast only had `session_id`+`name`, handler only logged chat. Fix: broadcast carries full `Session`, handler calls `app.add_session()`.
3. **State changes not applied** (`main.rs:599`, `state.rs:93`) — `SessionStateChanged` broadcast had string labels, handler only logged. Fix: broadcast carries `SessionState` enum, handler calls `app.update_session_state()`.
4. **tmux C-q detach broken** (`tmux.rs:26`) — `bind C-q` needs prefix, but `prefix None`. Fix: `bind -n C-q`.
5. **Orc brain blocked by plan mode** (`orc.rs:305`) — `--permission-mode plan` blocked MCP tools. Fix: `--dangerously-skip-permissions`.
6. **R restart incomplete** (`main.rs:357`) — created tmux session but didn't send `claude` command. Fix: added `send_text`+`send_keys`.

## Verified working

Startup, render, doctor, help/NewTask/ConfirmQuit modals + text input, orc brain responds with cost tracking, worker spawn via MCP (tab appears with ◐ badge), tab switching, worker tab renders session info, attach (Enter) → detach (C-q), graph toggle, confirm quit with running worker, clean exit + orphan cleanup.

## Remaining (untested live)

- **R restart** — needs a worker in Failed state. Code path wired up but never exercised live.
- **Review mode** — needs AwaitingReview state (orc calls `mark_done`). The `r` key, diff computation, split-pane review UI, approve/reject — all untested live.
- **PTY tail** — worker tab shows empty pty_tail pane. Pipe-pane logs exist but aren't read into the UI (no periodic polling of log file).
