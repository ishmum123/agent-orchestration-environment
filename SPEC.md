# Orc — Spec

A terminal app for running parallel Claude conversations with an orchestrator on top. You type to orc; orc plans, delegates to workers, and reports back. You can also talk to any worker directly. Everything happens inside one TUI — you never see a raw Claude session.

---

## Core principle

**Orc is the entire interface to a multi-Claude team.** Every conversation — the orchestrator and each worker — is a tab. You read tabs to see what's happening. You write to a tab through a pop-up. The orchestrator can act on its own, including answering workers' questions on your behalf.

There are exactly three kinds of participants:

- **Orc** — one. Plans, delegates, supervises.
- **Workers** — many. Each scoped to a subtask, isolated in its own git worktree.
- **You** — observe everything, intervene anywhere.

Workers are not terminals you attach into. They are Claude conversations rendered as a structured event log inside orc's UI.

---

## Tabs

One tab per conversation. The first tab is orc; subsequent tabs are workers, in spawn order.

Each tab shows:

```
[ session header — name, model, state, elapsed ]
[ scrollable event log ]
[ action bar — keys available in this context ]
```

The event log is append-only and chronological. It contains:

- Assistant text from this session
- Tool calls (collapsed: `→ Read({ ... })`)
- Tool results (collapsed: `← ok` or `← error: ...`)
- User messages sent into this session
- System notes — `[interrupted]`, `answered by orc: ...`, `worker exited code 0`, etc.

Tabs are scrollable with `j`/`k`. Switching is `Tab`/`Shift-Tab` or `1`-`9`. The orc tab is always `1`.

State badges in the tab strip: `◐` running · `?` blocked on permission · `!` blocked on a question · `◑` awaiting review · `✓` done · `✗` failed.

---

## Speaking

All input is a centered pop-up modal. There is no persistent input box on tabs. This keeps tabs purely about reading — writing is a deliberate act.

- **You → orc**: `t` from anywhere, or from the orc tab.
- **You → worker**: `t` from that worker's tab.
- **Orc → worker**: invisible — orc uses tools; you see the result land in the worker's log.
- **Worker → orc**: invisible — same; appears as a tool call in the worker's log.

Pop-ups belong to a session. Closing without sending is `Esc`; sending is `Enter`. Multi-line composition with `Shift-Enter` (newline in buffer); the modal grows up to a sensible cap.

---

## Questions

Any session can ask a question that requires a human-shaped answer. The question raises a pop-up on screen. **Who is allowed to answer depends on who's asking:**

- **Orc asks → only you can answer.** Orc cannot answer itself.
- **Worker asks → either you or orc can answer.** First responder wins; the pop-up dismisses regardless. The session that asked logs `answered by you: ...` or `answered by orc: ...` so the audit trail is clear.

For orc to actually answer a worker, orc has to *see* the question in real time. Worker questions are forwarded into orc's stream the moment they're raised. Orc decides per-question whether it has the context to answer, or stays quiet and lets you handle it.

If orc stays quiet, the user is the only path. The pop-up is non-blocking otherwise — you can switch tabs, read other logs, and come back. Tab badge flashes when a question is unresolved on a non-focused tab.

This is the orchestration loop in concrete form: orc supervises by *seeing* what workers do and ask, and *intervenes* when it has the context. You are the fallback, not the default.

---

## Interrupting

Any session — orc or any worker — can be interrupted from its tab with a single key (e.g. `Ctrl-C`). The session stops mid-thought; the conversation stays alive and ready for the next message. The tab logs `[interrupted]`.

Same gesture as Esc-during-Claude-Code, applied uniformly. There is no "kill the process and lose context" outcome from interrupt — that's what `k kill` is for.

---

## Spawning, killing, restarting

- **Spawn**: orc decides. You ask orc for work; orc spawns workers as it sees fit. (Direct user-initiated spawn from a key is possible but not the primary path.)
- **Kill (`k`)**: terminates a worker, removes its tab and worktree, drops its DB row. Confirmation modal first. Orc is notified via its event stream.
- **Restart (`R`)**: kills the current worker process and starts a fresh one in the same worktree, with conversation continuity (the new process resumes the prior Claude session). Used when a worker is wedged or after an interrupt didn't recover cleanly.

---

## Reviewing

When a worker has work ready for the user (worker calls `submit_for_review`, or user opens review on demand), the worker's tab supports three view modes that share one review session:

1. **Diff view** (`r`): default. Split-pane file tree on the left, diff on the right. Lowest friction. Most reviews end here.
2. **Whole-file view** (`o` from diff view): one keystroke up from diff. Single scrollable pane showing the file's *current* content with line numbers. Same review keys (`c` comment, `a` approve, `j/k` navigate). Used when context outside the changes matters.
3. **Editor (`e` from anywhere in review or worker tab)**: surrenders the terminal to `$EDITOR` pointed at the worktree (or a specific file:line). Orc resumes when the editor exits. Used when you're going to *change something*, not just read. The worker keeps running in the background — interrupt it first if you don't want it racing your edits.

**Comments and approvals accumulate in one review session regardless of view.** A comment placed in diff view is visible in whole-file view of the same file at the same line. Submit (`s`) sends everything to the worker as one structured payload; the worker resumes with the feedback as its next prompt. Cancel (`q`/`Esc`) leaves the review draft attached to the worker so you can come back.

The keystroke ladder `r → o → e` reinforces frequency and disruption: each step opens more capability and costs more.

---

## Scratch claude (`n`)

`n` (global, any tab, no modal open) surrenders the terminal to a plain `claude` child running in the project root and resumes orc when claude exits. Mirrors the `e` editor handoff: leave alt-screen, run blocking with inherited stdio, re-enter. One scratch session at a time.

Orc never sees the transcript. Nothing crosses the boundary by construction — the trust root stays sealed because there's no IPC, no log capture, no orc state touched. Workers in flight keep running in the background but are invisible while inside; that's an accepted trade-off for the scratch-session ergonomic.

The binary is resolved via the same `ORC_CLAUDE_BIN`-then-`claude` lookup workers use, so harness runs with the `fake_claude` shim work the same way.

---

## Scratch overlay (`?`)

`?` (global, no modal open) toggles a centred overlay containing a persistent backchannel claude conversation. ChatGPT-style: scrollable transcript on top, multi-line input box at the bottom (`Shift-Enter` for newline, `Enter` to send). `Esc` closes the overlay; the backchannel child keeps running so subsequent `?` presses re-open with prior conversation intact.

The backchannel is a stream-json claude child spawned in the project root cwd, on **sonnet** by default. No worktree, no MCP server attachment, no policy gating, no system prompt — it's a sealed-off sidekick for "ask a question, read some code, brainstorm" use that shouldn't pollute orc's context.

**Trust-root isolation.** The backchannel transcript never enters `orc_view.event_log`, never reaches the orchestrator, never appears in the agents panel or task graph. Workers spawned by orc don't see it either. The only escape hatch is `Ctrl-N` below, which is user-initiated — and it carries only a short distilled brief, not the chat itself.

The help screen lives on `h`. `?` is reserved for the overlay.

Keys, while overlay is open:

- `Esc` — close overlay; channel keeps running.
- `?` — also closes overlay when the input buffer is empty (so `?` toggles).
- `Ctrl-X` — kill the backchannel child and drop its state. Next `?` spawns a fresh sonnet channel.
- `Ctrl-B` — promote to opus. If the conversation is empty, just respawn on opus directly. If non-empty, the channel is asked for a 1–3 sentence summary; on completion the sonnet child is killed and a fresh opus child is spawned with the summary as its first user message (carried forward as a System line in the new transcript).
- `Ctrl-N` — **attach as worker.** Asks the channel for a `{slug, task}` summary; on completion the scratch child is killed, the overlay closes, and a fresh regular worker is spawned with the task as its brief. The new worker is structurally indistinguishable from one spawned via `c` chat → `spawn_session`: own worktree, MCP wiring, worker system prompt, tab in the panel. Focus jumps to the new tab. The scratch chat history is **not** transferred — only the summary crosses the boundary. The trade-off (accepted): faithfulness to the prior chat is lost; true worker-equivalence is gained.

Summary prompt: the channel is told to reply with strict JSON on a single line — `{"slug":"<kebab-case ≤20 chars>","task":"<1-3 sentence brief>"}`. The first balanced `{…}` is extracted (so a code-fenced or prose-wrapped reply still parses), the slug is sanitized to `[a-z0-9-]` and truncated, and the task is taken verbatim. **Fallback** (any parse failure or empty fields): slug becomes `scratch-<HHMMSS>` and the entire raw reply becomes the task. This keeps the flow robust to a non-cooperating model (e.g. the echo-mode `fake_claude` shim used in harness tests).

Edge cases:

- **Ctrl+N during an in-flight turn.** The summary request is queued onto the channel's stdin — claude processes user messages serially, so it runs naturally after the current turn completes. No abort, no race.
- **Ctrl+N with an empty conversation.** No-op except for an inline system line in the overlay (`nothing to attach — say something first.`). The channel is not killed; the user keeps the overlay.
- **Spawn failure** (e.g. worktree creation fails because of a dirty repo). The overlay stays open and a system line records the error. The channel is not killed — the user's chat is preserved so they can retry or copy it out.
- **Name collision.** If the model picks a slug already in use by an existing session, orc appends `-2`, `-3`, … until a free name is found, so `git worktree add` doesn't fail.

The binary is resolved via the same `ORC_CLAUDE_BIN`-then-`claude` lookup workers use, so harness runs with the `fake_claude` shim work the same way.

## What's preserved

- **Worktrees**. Each worker gets `git worktree add` to a dedicated branch. This is workspace isolation, unrelated to how the Claude process is run. Worktrees stay.
- **State machine**. Running · Blocked · AwaitingReview · Done · Failed. Same as before.
- **Persistence**. Sessions, transitions, reviews, task graphs in SQLite. Survives restarts.
- **`orc doctor`**. Sanity check for git, claude CLI, and any leftover artefacts.

## What's removed

- **tmux for workers**. Workers are stream-json children of orc; no terminal involved.
- **Attach-into-claude for workers**. No path surrenders the screen to a worker's Claude TUI; workers are stream-json children, observed only through orc. The scratch `n` handoff is *not* this — it spawns a fresh, unrelated claude session and never sees orc's worker state.
- **The PTY tail / pane snapshot**. Workers' logs are structured events, not terminal redraws.

---

## What you see

The promise to the user, in order of importance:

1. **Always inside orc.** Every Claude conversation is rendered by orc. The only thing that ever takes the screen away is your editor, and only when you ask for it.
2. **Readable, not noisy.** Workers' tabs show what they think and do, in chronological order, structured. No status-line redraws, no cursor-positioning artefacts.
3. **One mental model everywhere.** Tabs work the same way whether the conversation is orc or a worker. Pop-ups, questions, interrupts, review — all behave identically across tab types.
4. **Orchestration is real.** Orc isn't a router for messages. It supervises workers, answers their questions when it can, and only escalates to you when it can't.
5. **You can always intervene.** Type to any tab. Interrupt any tab. Review or edit any worker's worktree. The orchestrator has authority over workers; you have authority over everything.

---

## Out of scope (this spec)

- Multi-orc / nested orchestration.
- Workers spawned from another runtime (Codex, etc.) — interface designed for it but only Claude is implemented.
- Cross-session conflict detection beyond worktree paths.
- Web UI, daemon mode, mobile.
- Memory across orc runs beyond what SQLite holds.
