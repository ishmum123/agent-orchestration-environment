# orc

Agent Orchestration Environment (AOE). A TUI that runs multiple Claude Code sessions as agents, with one orchestrator coordinating them.

## Why

My workflow has shifted to running Claude Code in the terminal and using the IDE as a diff viewer. Running two or three Claude sessions in parallel for multi-tasking made one thing obvious: the bottleneck moves from writing code to jumping between tabs answering trivial questions — which library, which file, which name.

Claude Squad solves seeing all sessions at once. Anthropic's Agent Teams automates more of the orchestration. Neither lets me both hand off the routine work *and* drop into a session directly when I want to.

orc is my attempt at that. A central orchestrator I talk to, which spawns workers, handles the small stuff itself, and only pulls me in when something needs a human. I can also jump into any worker's tab and talk to it directly.

Proof of concept more than a product. MIT licensed.

## Install

    git clone https://github.com/ishmum123/agent-orchestration-environment orc
    cd orc
    cargo install --path . --force

Or grab a prebuilt binary from the latest green `main` CI run (via [nightly.link](https://nightly.link/), no GitHub login needed):

- Linux x86_64: [orc-linux-x86_64.zip](https://nightly.link/ishmum123/agent-orchestration-environment/workflows/build/main/orc-linux-x86_64.zip)
- macOS arm64: [orc-macos-arm64.zip](https://nightly.link/ishmum123/agent-orchestration-environment/workflows/build/main/orc-macos-arm64.zip)

Unzip, then `chmod +x orc`.

Requires:

- Rust (stable)
- `claude` CLI on `$PATH` ([Claude Code](https://claude.com/claude-code)), authenticated
- `git`
- `tmux` (only used by the test harness, not by orc itself)

Sanity check:

    orc doctor

## Run

    orc                       # current directory
    orc -p /path/to/project   # specific project

First time, orc opens a talk modal: "describe the task for orc to plan." Type a goal. orc plans, spawns workers, and you watch.

## Keys

| Key | Does |
| --- | --- |
| `t` | Talk to the focused tab (orc or a worker) |
| `Tab` / `Shift-Tab` | Cycle tabs |
| `1`–`9` | Jump to tab N (orc is always 1) |
| `j` / `↓` | Scroll log down |
| `k` / `↑` | Scroll log up |
| `g` / `G` | Top / bottom of log |
| `r` | Review focused worker's diff |
| `x` | Kill focused worker (drops tab and worktree) |
| `R` | Restart a wedged worker (resumes its session) |
| `Ctrl-C` | Interrupt the current session (conversation survives) |
| `?` | Toggle help overlay |
| `Esc` | Close modal / dismiss |
| `q` | Quit |

In review (`r`): `c` comment · `a` approve · `j`/`k` navigate · `o` whole-file · `e` open in `$EDITOR` · `s` submit · `q` cancel.

Tab strip badges: `◐` running · `!` blocked on question · `?` blocked on permission · `◑` awaiting review · `✓` done · `✗` failed.

## How it works

- **Workers are stream-json `claude` children of orc.** No tmux, no PTY scraping. Their output is parsed into a structured event log per tab.
- **State lives in SQLite** at `~/.config/orc/state.db`. Sessions, transitions, reviews — all there. Survives restarts.
- **Each worker gets `git worktree add`** to a dedicated branch under `orc/<task>`. They can't step on each other or on your working tree.
- **orc itself is a Claude session** with a small MCP server exposing `spawn_worker`, `answer_worker_question`, `current_summary`, etc. It supervises by *using tools*, not by parsing screens.
- **Hooks + a policy engine** gate dangerous actions. Hard-deny rules (`rm -rf /`, modifying secrets) are not overridable; soft rules are configurable.

Full architecture spec in [SPEC.md](SPEC.md).

## Status

Pre-1.0. Architecture is in place, happy path works, rough edges tracked in [docs/superpowers/plans/](docs/superpowers/plans). Issues and PRs welcome.

## License

[MIT](LICENSE).
