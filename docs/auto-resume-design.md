# Auto-resume on Claude usage-limit reset — design

Captures research + design decisions for the queued *"Auto-start task when usage limit reached"* TODO. Three implementation attempts stopped before coding for structural reasons; the findings are recorded here so the next pass can skip research and design and go straight to implementation.

---

## The problem

When the user's Claude plan hits a usage cap mid-run, every claude child (workers AND the orchestrator brain) stalls together — they share one Anthropic account. orc has no detection or recovery path today, so sessions sit stuck until the user manually restarts.

Constraints from the user:

- Resume logic must live in pure Rust orc code, not in the orchestrator LLM (the brain itself is also rate-limited).
- Backoff range: **5 minutes minimum → 5 hours maximum**.
- Indefinite retry at the 5h ceiling; never transition a session to `Failed` from a quota cap.

---

## Detection — the signal claude emits

claude code emits a plain **assistant text content block** matching:

```
^Claude AI usage limit reached\|(\d+)(?:\|.+)?$
```

- The captured integer is the unix epoch (seconds) when the cap lifts — call this `vendor_resume_at`.
- The optional third pipe segment (`session`, `weekly`, etc.) is informational; treat all hits identically.
- The signal appears inside an `assistant` stream-json event, in `/message/content[]` where `block.type == "text"`.

Confirmed against six independent third-party detectors: cline, happy (slopus), hapi (tiann), ccusage, andrepimenta/claude-code-chat, OpenLAIR/dr-claw. **No authoritative Anthropic doc** — SPEC must note "observed in the wild as of 2026-05".

Sources:
- https://github.com/anthropics/claude-code/issues/2087
- https://github.com/cline/cline/issues/4616
- https://github.com/tiann/hapi
- https://github.com/slopus/happy
- https://code.claude.com/docs/en/agent-sdk/sessions (for `--resume`)

---

## State model — global, not per-session

A new global `QuotaState` lives on `StateManager`:

```rust
pub enum QuotaState {
    Healthy,
    Waiting {
        resume_at: DateTime<Utc>,
        retry_count: u32,
        vendor_resume_at: Option<DateTime<Utc>>,
        at_ceiling: bool,
    },
}
```

A new session state surfaces the pause in the UI:

```rust
SessionState::WaitingForQuota   // no fields; UI reads from global QuotaState
```

**Why global, not per-session.** The cap is account-wide and time-bound. If only the first-tripping session pauses, the next sessions trip the limit one by one over the following seconds, each spawning its own retry timer. Global pause collapses bookkeeping into one `resume_at`, one ladder, one source of truth.

**Honest-pause rule.** When the global flips to `Waiting`, *only* sessions with an in-flight turn at that moment flip to `WaitingForQuota`. Idle sessions stay `Running`; their next user message gets queued (see "User input"). Sessions in `Blocked` or `AwaitingReview` are unaffected — the cap doesn't change their meaning, only the next-API-call possibility.

---

## Session events

```rust
SessionEvent::QuotaPaused        // Running → WaitingForQuota
SessionEvent::QuotaResumed       // WaitingForQuota → Running
```

No `RetryExhausted` — sessions never fail from quota.

---

## Global commands (StateCommand additions)

```rust
StateCommand::QuotaHit {
    vendor_resume_at: Option<DateTime<Utc>>,
    source_session: String,
}
StateCommand::QuotaCleared
```

Behavior on `QuotaHit`:

1. Set `QuotaState::Waiting` with `resume_at = max(vendor_resume_at, now + backoff_for(retry_count))`. Vendor floor applied **only when `retry_count == 0`** (first hit of this episode); subsequent backoff is orc-authoritative since vendor timestamps have known reliability issues.
2. For every session with an in-flight turn, apply `QuotaPaused` and kill its child.
3. Broadcast `StateChange::QuotaStateChanged`.

`QuotaCleared` fires after 30 min of `Healthy` with at least one successful turn anywhere — resets `retry_count` to 0 so one bad hour doesn't leave you stuck at 5h backoff for the rest of the day.

---

## Backoff ladder

```rust
fn wait_for(retry_count: u32) -> Duration {
    let mins = match retry_count {
        0 => 5,   1 => 10,  2 => 20,  3 => 40,
        4 => 80,  5 => 160, _ => 300,   // ceiling = 5h, forever
    };
    Duration::from_secs(mins * 60)
}
```

`retry_count` increments on every `QuotaHit` while already `Waiting` (a resume that re-tripped). Once `retry_count >= 6`, `at_ceiling = true` and the UI surfaces a `· ceiling` token.

**Ceiling behavior**: retry forever at 5h. Never auto-fail. User can `R` force-resume or `k` kill at any time.

---

## Scheduler

Single tokio task with 10-second tick (cheap; gives ±5s precision on resume timing):

```
every 10s:
  if QuotaState::Waiting and now >= resume_at:
    1. respawn orc brain (use existing claude_session_id if any, fresh otherwise)
    2. for each WaitingForQuota worker in spawn order, spawn_worker_resume
       with 5s gaps between them
    3. drain pending_user_messages queue in order
       (orc first, then per-worker as their children come online)
    4. flip QuotaState to Healthy (provisionally)
```

If a respawned child trips the limit on its first turn, `QuotaHit` fires again → `retry_count += 1`, kill any other children already respawned in this wake-cycle, back to `Waiting`. The 5-second stagger between workers means at most one wasted API call per re-trip (rather than N parallel ones).

---

## User input queueing

When `QuotaState::Waiting` and the user submits a message via `t` (any tab) or `c` (new task):

- Append to `pending_user_messages: Vec<(SessionId, String)>` in `StateManager`.
- Render in event log as `you → ...` dimmed with a `(queued)` tag.
- System line in tab: `· queued — Claude usage limit, will send at HH:MM`.
- On resume, scheduler drains in order before any further API activity.

**Why queue, not block.** The whole point of orc is "always intervene." Locking the user out for 5 hours is unacceptable. Letting them type and queue means they keep working with the same mental model they have when a worker is `Blocked` — and orc's job is to ferry the queue across the resume boundary.

**Exceptions:**

- **Scratch overlay (`?`)** during `Waiting`: overlay opens but refuses to spawn the backchannel child. Shows the global banner instead of the input box. No queue for scratch — messages are throwaway by design.
- **`n` fullscreen** during `Waiting`: refuse the keystroke, show a brief toast.

---

## UI

- **Tab strip glyph**: `◌` for `WaitingForQuota`. Same for the orc tab when its brain is paused.
- **Agents panel row**: `◌ name  waiting · 12m` or `◌ name  waiting · 4h 58m · ceiling`.
- **Worker/orc tab header**: `paused — Claude usage limit, resumes at 15:42 (retry 3)`. The `(retry N)` is useful forensics.
- **Worker tab event log**: system line `· paused — usage limit hit, queued until 15:42` at pause; `· resumed` on resume. Queued user messages render as normal `you → ...` lines but dimmed with a `(queued)` tag until actually sent.
- **Action bar on a paused tab**: `R force-resume now` · `k kill`.
- **Global banner** above the tab strip when `QuotaState::Waiting`:
  `⏸ Claude usage limit · resumes at 15:42 · 3 sessions waiting`
  Replaces per-tab clutter when many sessions are paused. Vanishes when `Healthy`.

---

## fake_claude shim — test affordances

Extend `src/bin/fake_claude.rs`:

- `FAKE_CLAUDE_USAGE_LIMIT=1` → on the first N invocations (say N≤3), emit `Claude AI usage limit reached|<now+RESET_SECS>` as an assistant text block, then a `result` event with `subtype: "error"`. After N, behave normally.
- `FAKE_CLAUDE_USAGE_RESET_SECS=N` (default 60) → controls the embedded timestamp.
- **Shared counter** at `/tmp/orc-fake-claude.count` (NOT per-session) so orc brain + workers all hit on first turns — modeling account-wide behavior.

## Orc-side test compression

`ORC_QUOTA_BACKOFF_SCALE=0.01` env var multiplies the entire backoff ladder. With this, 5 min becomes 3 s — required for harness use. Compile-time `#[cfg(test)]` would also work; env is simpler for tmux harness.

---

## Resume mechanics

orc already captures `claude_session_id` per worker via the `system` event (`WorkerEvent::ClaudeSessionId`), and `spawn_worker_resume(...)` already passes `--resume <id>`. **No worker.rs refactor needed for resume itself.**

---

## Structural conflicts found during implementation attempts

The third implementation attempt stopped after reading the code with two design-vs-code conflicts:

1. **`app.rs` has no async loop.** The original brief said "scheduler tokio task in `app.rs`"; structurally impossible — `app.rs` is a pure data container. Scheduler must live in `main.rs` alongside the existing `tokio::select!` loop, or be a standalone `tokio::spawn` started in `main()`.

2. **No "in-flight turn" tracking exists today.** The honest-pause rule requires per-session `is_busy: bool` tracked at user-send (`true`) and `WorkerEvent::Result` (`false`). This requires extending `SessionView` and touching the `WorkerEvent` dispatch path in `main.rs` around the `handle_worker_event` site.

Both are fixable, but they imply the implementer must touch `main.rs::handle_worker_event` — the same hot path sibling work (image attachments, etc.) was modifying. Sequencing matters.

---

## Implementation order (bottom-up)

1. Read first: `src/app.rs`, `src/ui/mod.rs`, `src/ui/panel.rs`, `src/ui/worker.rs`, `src/db.rs`, `src/state.rs`, `src/session.rs`, `src/worker.rs`, `src/main.rs`.
2. Add `QuotaState` struct + `backoff_for(retry_count)` fn.
3. Add `SessionState::WaitingForQuota` variant. Update every exhaustive match site (~30): `transition()`, `state_label`, `sweep_zombie_sessions`, `db.rs::update_session_state`, UI badge/panel/tabs/worker renderers.
4. Add `StateCommand::QuotaHit` and `QuotaCleared` to `StateManager`.
5. Add the magic-string detector in `worker.rs::parse_worker_events` (regex match on assistant text blocks). Route to `QuotaHit`.
6. Add per-session `is_busy` flag. Toggle on user-send (`true`) and `WorkerEvent::Result` (`false`).
7. Add `pending_user_messages` queue in `StateManager`. Wire the send paths to enqueue when `Waiting`.
8. Add the scheduler tokio task in `main()` (not `app.rs`). 10s tick. Wake → respawn orc brain → respawn workers with 5s stagger → drain queue → flip to `Healthy`.
9. Extend `fake_claude.rs` with `FAKE_CLAUDE_USAGE_LIMIT` + `FAKE_CLAUDE_USAGE_RESET_SECS` + shared counter.
10. Implement `ORC_QUOTA_BACKOFF_SCALE` env knob.
11. UI: `◌` glyph, countdown, global banner, `(retry N)`, `· ceiling` token, action-bar `R force-resume now`.
12. SPEC.md "Auto-resume on usage limit" section — include the magic-string regex and the in-the-wild caveat.
13. Unit tests: parser, transitions, backoff ladder, scheduler tick, queue drain.
14. Live-verify in tmux with the extended fake_claude. Capture-pane at: limit-hit, paused panel + banner, auto-resume, queued message delivery, `R` force-resume, ceiling display, clean exit.

---

## Recommended dispatch

A single agent at full budget, briefed with this document as the entire design (no research, no design phase). Sibling agents touching `worker.rs` should be landed first to avoid three-way merge conflicts on `main.rs::handle_worker_event` and the `WorkerEvent` dispatch surface.

If a single agent still can't fit the 14-step implementation + live-verify into one budget, split as:

1. State-machine + StateManager + worker.rs regex (foundation).
2. Scheduler + queue + main.rs wiring + fake_claude env vars.
3. UI + SPEC.md + live-verify.

Each sub-agent must land cleanly before the next starts.
