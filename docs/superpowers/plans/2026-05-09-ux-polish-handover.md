# UX Polish Handover

**Branch:** `v2-rewrite` (HEAD `0deb457`, pushed). All prior work shipped — this doc covers fixes from a live-usage review.

**Read first:** `SPEC.md`, `CLAUDE.md`. Architecture is in place; this is polish + a handful of small features.

**Verification rule (from CLAUDE.md and saved memory):** After any non-trivial change, run the tmux harness (`tst-` session prefix) yourself. Don't claim done from unit tests alone. Confirm pane render, EXIT=0, no orphan `orc`/`claude` processes, `orc doctor` green. For changes touching workers, drive a real worker spawn.

---

## Issues to fix (each its own commit unless noted)

### 1. Right-side agents panel: text clipped/garbled

Screenshots consistently show the orc card rendering as `ntalManager5hing why Credential` and the worker card as `ntalManager5thManager.kt and res`. Leading characters cut, stray `5` in the middle.

Likely cause: width math in the panel renderer doesn't account for the highlight bar (`▌` + space) or the card indent, and a previous longer string's tail is bleeding through because the area isn't being cleared between cards. Inspect the panel render fn (search `▌` in `src/ui/`); rewrite to:
- Compute usable width = panel_width − left_indent − right_pad.
- Wrap summary with `textwrap` (already a transitive dep via ratatui? if not, do it manually word-aware) to ≤2 lines.
- Truncate overflow with `…`.
- Clear the entire card area with spaces before drawing (to avoid leftover characters from prior frames).

Acceptance: full multi-line summary visible, no leading-char truncation, no stray digits/letters, no characters cross the panel border.

### 2. Event log overflows the right border

Long lines extend past the panel divider into the agents panel. The events `Paragraph` is rendered without wrap — set `.wrap(Wrap { trim: false })` on the paragraph in the worker/orc tab event log render path (probably `log_lines` in `src/ui/worker.rs` or its caller).

Acceptance: no event-log line crosses into the right panel; long lines wrap inside the events box.

### 3. Drop the cost from `— turn end —`

User: "subscription, redundant". Find every place that builds `LogEntry::TurnEnd { cost_usd }` rendering and drop the `($X)` portion. Keep the `— turn end —` divider.

Implementation: in the renderer, ignore `cost_usd`. Field can stay on the variant for now (cheap to leave). If you want to drop it entirely, do that — caller side would also need updating.

Acceptance: turn end shows only `— turn end —`.

### 4. Stale repeated `[orc model: ...]` after every turn

`[orc model: claude-opus-4-7]` appears on every turn in the orc log. It should appear once at startup, not on every `system` event. In `src/main.rs::handle_orc_event` `OrcEvent::System`, only push the model line if the model actually changed (track last-seen on `OrcView`). Or just push it once on first system event ever (track a `bool` on OrcView).

Acceptance: orc model line appears once near top; not repeated.

### 5. `(thinking)` noise

Bare `(thinking)` lines appear repeatedly with no body, sometimes consecutively. Either the thinking text is empty (skip those entirely — don't push the LogEntry if `text.is_empty()`) or coalesce consecutive thinking blocks visually.

Implementation: in `src/main.rs` (or wherever `WorkerEvent::Thinking` and `OrcEvent::Thinking` are converted to `LogEntry::Thinking`), skip if `text.trim().is_empty()`. Additionally, in the renderer, if multiple consecutive `LogEntry::Thinking` exist, fold them into one `(thinking) <first 80 chars of concatenated>…` line.

Acceptance: no bare `(thinking)` rows; thinking text appears with content or not at all.

### 6. Tool-input JSON noise

Both worker and orc logs show full JSON dumps for `Bash`, `WebFetch`, `WebSearch`, etc. Render them as a one-line summary instead.

Implementation: Replace the current `truncate_json` summarisation at the LogEntry conversion site with a tool-aware formatter. Examples of desired output:
- `Bash` → `Bash: <first 80 chars of input.command>`
- `WebFetch` → `WebFetch: <input.url or input.prompt[:60]>`
- `WebSearch` → `WebSearch: <input.query>`
- `Read` → `Read: <input.file_path>`
- `Edit` → `Edit: <input.file_path>`
- Unknown → `<name>(<truncate_json(input, 60)>)` (current behavior).

Add a `pub fn summarize_tool_input(name: &str, input: &serde_json::Value) -> String` in `src/ui/worker.rs` (next to `log_lines`) and call it from both `handle_orc_event` and `handle_worker_event`.

Acceptance: tool calls in the log read like one-line human descriptions, not JSON dumps.

### 7. Self-MCP filter is incomplete

`ToolSearch`, `ScheduleWakeup`, and `mcp__orc__instruct_session` / `mcp__orc__mark_do…` (truncated) appear in the orc log even though they should be hidden. The current filter (`ORC_MCP_TOOL_NAMES` in `src/mcp.rs`) only catches a handful.

Audit `src/mcp.rs` for the complete tool registry and update `ORC_MCP_TOOL_NAMES` to include every tool the orc-side server registers. Plus add the harness-level meta tools that show up in the screenshots:
- `ToolSearch` — definitely hide.
- `ScheduleWakeup` — definitely hide.
- Any other harness/meta tool exposed via MCP that's an internal control surface, not user-facing work.

If those latter tools come from the *Claude harness* (not orc's own MCP server), filter them by name regardless. Add a separate constant `pub const HARNESS_META_TOOLS: &[&str] = &["ToolSearch", "ScheduleWakeup", "TaskCreate", "TaskUpdate", "TodoWrite", ...];` and check both lists.

Acceptance: orc's log no longer shows `ToolSearch`, `ScheduleWakeup`, or any `mcp__orc__*` call.

### 8. Markdown rendered raw

Worker assistant text dumps `**bold**`, `###`, `|table|` syntax verbatim. Two acceptable approaches — pick one:
- **Minimal**: strip a few markdown markers when rendering assistant text — bold `**x**` → just `x` (or styled bold via ratatui Modifier::BOLD), headings strip the leading `#`s and bold the line, tables left as-is (hard to reformat). Use a tiny custom pass; don't pull a markdown crate just for this.
- **Better**: use the `pulldown-cmark` crate (small, no native deps) to parse the markdown into events and render styled `Line`s. More robust for tables and nested formatting.

Recommend the minimal approach now (single commit, ~50 lines) — escalate to pulldown-cmark only if the user dislikes the result.

Acceptance: bold renders bold (or unstyled but un-asterisked), headings render as bold standalone lines, no `**` or `###` characters bleeding through into prose.

### 9. Scrolling keys + auto-follow

Currently `j/k/Up/Down/PageUp/PageDown` are wired but `k` collides with kill. And there's no autoscroll-to-bottom-unless-manually-scrolled behavior, no jump-to-end key.

Plan:
- Remove `k` as a scroll key (kill keeps `k`). Use `j`/`Down` to scroll down, `Up` to scroll up. Drop the `Char('k')` arm of the scroll handler.
- Add `G` (capital, vim-style) for "jump to end".
- Add `gg` (vim-style) for jump to top, OR just `Home`/`End` keys.
- Auto-follow: track a `bool stick_to_bottom` per session view (default `true`). On every new `LogEntry`, if `stick_to_bottom`, set scroll to `event_log.len().saturating_sub(visible_height)`. Any manual scroll up sets `stick_to_bottom = false`. Pressing `G` re-enables it.
- Apply same auto-follow to orc tab.

Acceptance: log auto-tails by default; scrolling up freezes the view; `G` jumps back to end and re-enables follow; `k` no longer scrolls.

### 10. Action bar should be state-aware

Currently shows `r review` even when state is Running, etc. (Sometimes correct, sometimes not.) Audit `src/ui/mod.rs::render_action_bar` and only show:
- `r review` when `SessionState::AwaitingReview`
- `R restart` when `SessionState::Failed`
- `^C interrupt` only when `Running` (not when AwaitingReview/Done/Failed)
- `t talk` always (see #11)
- `1-9 tab`, `q quit`, `? help` always
- Add `G end` (jump to bottom of log) always
- `o whole-file`, `e editor`, `s submit`, `c comment`, `a approve`, `q cancel` only in review mode (likely already handled by review key handler)

Acceptance: action bar only shows applicable keys; no stale `r` on running tabs.

### 11. Rebind `n` → `t` for "talk"

User wants `t talk` instead of `n speak`. `c` must remain free for control (Watch↔Control mode toggle).

- Replace `KeyCode::Char('n')` arm with `KeyCode::Char('t')` in `src/main.rs::handle_key`.
- Update `Modal::NewTask` modal title hint and the action bar label.
- Update the welcome modal trigger if it references `n`.
- Update help text in `src/ui/modals.rs::render_help`.
- Update commit-body acceptance docs / SPEC.md if they mention `n` for input. (SPEC.md uses `n` — change to `t`.)

Acceptance: pressing `t` opens the talk modal; pressing `n` does nothing; help shows `t talk`; SPEC reflects the change.

### 12. Visual spacing between assistant responses

Two consecutive assistant turns currently run together with only `— turn end —` between them. Add a blank line *after* the turn-end divider so the next user/assistant block visually separates. Tiny renderer change in `log_lines`.

Acceptance: clear visual gap between turns; not just a one-line divider.

### 13. Scroll indicator

No way to tell from the screen whether there's more content above or below the visible region. Add a minimal indicator in the events box title or a thin scrollbar-style glyph column on the right edge of the events area:
- Title becomes `events  [N–M / TOTAL]` where N–M is the visible range, or
- A `↑` glyph in the top-right corner of the events box if scroll>0, and `↓` in the bottom-right if not at end.

Pick whichever is simpler. Apply to both orc and worker tabs.

Acceptance: user can tell at a glance whether scrolling up/down would reveal more.

### 14. Card separation in the agents panel

Orc card and worker cards run together with no clear divider. The focused-card `▌` highlight is the only visual differentiator. Add:
- A blank line between every card (not just before/after focused).
- Make the badge column left-aligned consistently — orc's `◐` and the worker's `◐`/`✓` should sit in the same column. (Right now from screenshots it looks like the highlight bar pushes the focused card's contents one column right of the unfocused card's, breaking alignment.)
- Optional: a faint separator line (`─` × panel_width) between cards if blank-line spacing isn't enough.

Acceptance: cards are clearly distinct units; badge column is consistent regardless of focus state.

---

## Larger design — defer to a separate plan

### A. Orc actually engages with worker reviews

Today, when a worker enters AwaitingReview, orc sees a system note (`[session X: AwaitingReview]`) and responds by polling with `ScheduleWakeup` indefinitely (visible in screenshot 3). That defeats the orchestrator's purpose.

What's needed (write a separate plan in `docs/superpowers/plans/orc-review-engagement.md` and discuss with user before implementing):
- An MCP tool orc can call to *open the review session*: read the diff, read the worker's submitted summary, optionally inspect the worktree files. Could be `inspect_review(session_id)` returning `{diff, summary, files_changed}`.
- An MCP tool orc can call to *submit the review*: `submit_review(session_id, approve|comments|request_changes)` that does what the user's `s` keypress does today.
- System prompt clause that tells orc to actually use these when a worker hits AwaitingReview, not to poll.
- User retains override authority — if user opens the review first via `r`, orc backs off (or vice versa: first-responder wins, mirroring the question-answering pattern).

This is a real feature, not a polish task. Don't pile it onto the polish commit batch.

### B. Strip the cost field from `LogEntry::TurnEnd` entirely

If user permanently doesn't want cost, also remove the field from the variant — clean rather than ignoring. Low risk; do it whenever convenient.

---

## Process

- One commit per numbered item (1–11). Don't bundle.
- `cargo build && cargo test` clean before each commit.
- After all commits, run the tmux harness end-to-end: launch orc, send a chat to orc with `t`, capture pane, confirm welcome modal opens, agents panel renders cleanly (no garbled text), event log doesn't overflow, scroll keys work, `G` jumps to bottom. Then `q` + `y` to exit. Confirm EXIT=0, no orphans, doctor green.
- Final commit: `test: live-verified UX polish pass` with capture excerpts in body.
- Don't push until user asks.

## Files most likely involved

- `src/ui/mod.rs` — action bar, layout, agents panel root
- `src/ui/worker.rs` — `log_lines`, event-log paragraph wrap
- `src/ui/modals.rs` — modal titles/hints, help text
- `src/main.rs` — keybindings, scroll state, autoscroll, model-line dedup, thinking skip
- `src/mcp.rs` — `ORC_MCP_TOOL_NAMES`, add `HARNESS_META_TOOLS`
- `src/app.rs` — `OrcView`/`SessionView` add `stick_to_bottom`, `last_seen_orc_model`, etc.
- `SPEC.md` — `n` → `t` reference

## Known gotchas

- `cargo test` has a few warnings (60+) but builds clean. Don't try to fix unrelated warnings.
- Welcome modal opens automatically on startup — when the harness sends keys, account for that (send Escape first if you don't want to type into it).
- Filter must catch both bare names (`Bash`, `Read`) and prefixed forms (`mcp__orc__spawn_session`) — existing helper `is_orc_mcp_tool` handles prefixed; extend or add a sibling for harness tools.
- The current `(thinking)` skip in `src/ui/worker.rs::log_lines` may or may not check empty — verify.
