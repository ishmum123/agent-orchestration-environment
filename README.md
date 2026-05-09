# orc

Run a team of Claude Code agents from a single terminal. One orchestrator, many workers, all on screen at once.

You type to **orc**. Orc plans, spawns **workers**, delegates subtasks, watches what they do, answers their questions when it can, and bothers you when it can't. Each worker runs in its own git worktree on its own branch, so nothing collides. Every conversation — orc and every worker — is a tab in one TUI. You never see a raw Claude session.

```
┌─ 1 orc ◐ ─┬─ 2 auth-refactor ◑ ─┬─ 3 add-tests ◐ ─┬─ 4 docs ✓ ─┐
│                                                                │
│  orc → spawned worker "auth-refactor" on branch orc/auth       │
│  orc → spawned worker "add-tests" on branch orc/tests          │
│  → answer_worker_question({ id: 17, answer: "use jwt" })       │
│  ← ok                                                          │
│  worker docs finished — review pending                         │
│                                                                │
└─ [t] talk  [Tab] next  [r] review  [k] kill  [q] quit ─────────┘
```

## Why

Running parallel Claude agents in separate terminals doesn't scale past two. You lose track of who asked what, you context-switch between panes to read logs, and there's nothing supervising the agents — every question hits *you*.

Orc is the supervisor layer. It reads the workers' streams in real time, answers routine questions on your behalf, and only escalates the ones that actually need a human. You're the fallback, not the default.

## Install

```bash
git clone https://github.com/ishmum123/agent-orchestration-environment orc
cd orc
cargo install --path . --force
```

Requires:

- Rust (stable)
- `claude` CLI on `$PATH` ([Claude Code](https://claude.com/claude-code))
- `git`
- `tmux` (only used by the test harness, not by orc itself)

Sanity check:

```bash
orc doctor
```

## Run

```bash
orc                       # current directory
orc -p /path/to/project   # specific project
```

First time, orc opens a "what do you want to build?" modal. Type a goal. Orc plans, spawns workers, and you watch.

## Keys

| Key | Does |
|---|---|
| `t` | Talk to the focused tab (orc or a worker) |
| `Tab` / `Shift-Tab` | Next / previous tab |
| `1`–`9` | Jump to tab N (orc is always 1) |
| `j` / `k` | Scroll log down / up |
| `r` | Review worker's diff |
| `o` | Whole-file view (from review) |
| `e` | Hand the terminal to `$EDITOR` |
| `Ctrl-C` | Interrupt the current session (conversation survives) |
| `R` | Restart a wedged worker (resumes its session) |
| `k` | Kill a worker (drops the worktree) |
| `q` | Quit |

## How it works

- **Workers are stream-json `claude` children of orc.** No tmux, no PTY scraping. Their output is parsed into a structured event log per tab.
- **State lives in SQLite** at `~/.config/orc/state.db`. Sessions, transitions, reviews — all there. Survives restarts.
- **Each worker gets `git worktree add`** to a dedicated branch under `orc/<task>`. They can't step on each other or on your working tree.
- **Orc itself is a Claude session** with a small MCP server exposing `spawn_worker`, `answer_worker_question`, `current_summary`, etc. It supervises by *using tools*, not by parsing screens.
- **Hooks + a policy engine** gate dangerous actions. Hard-deny rules (rm -rf /, modifying secrets) are not overridable; soft rules are configurable.

The full architecture spec is in [SPEC.md](./SPEC.md).

## Status

Pre-1.0. The architecture is in place, the happy path works, the rough edges are in [docs/superpowers/plans/](./docs/superpowers/plans/). Issues and PRs welcome.

## License

[MIT](./LICENSE).
