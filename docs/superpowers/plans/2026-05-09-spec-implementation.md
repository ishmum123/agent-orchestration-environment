# Orc SPEC Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Bring the codebase in line with `SPEC.md` — workers run as stream-json children (no tmux), the orc tab is rendered like a worker tab (structured event log), input is always a centered pop-up, worker questions can be answered by orc or you (first-responder), the review flow gains the diff/whole-file/editor ladder, and `R` restarts a worker with conversation continuity.

**Architecture:**
- Workers are `WorkerHandle` (stream-json child) keyed in a `WorkerRegistry` owned by the event loop. Drop tmux from worker lifecycle entirely; tmux remains only as test infrastructure (see CLAUDE.md). The `tmux_session` column stays in DB for migration safety but is never used.
- The orc tab is a peer of worker tabs — same `EventLog` structure, same renderer. Orc's `OrcEvent`s and worker's `WorkerEvent`s both feed an append-only `Vec<LogEntry>` per session. The dashboard module disappears.
- Modal input is the only input surface. `n` is universal "speak to focused tab"; `Ctrl-C` interrupts focused tab; `Esc` always cancels modal.
- Questions: workers asking via `ask_user` MCP tool fire a `StateChange::UserQuestionPending` AND simultaneously inject the question into orc's stdin so orc sees it. Either party can answer; `state_handle.answer_user` is idempotent (second answer is no-op once a question is closed).
- Review: `ReviewState` gains a `view_mode` field (`Diff` | `WholeFile` | `Editor`). `r` enters diff, `o` toggles to whole-file, `e` shells out to `$EDITOR`, `s` submits, `q`/`Esc` keeps draft.
- Restart: `R` calls `claude --resume <session-uuid>` against the worker's stored Claude session id (captured from `system` event with `session_id` on first turn).

**Tech Stack:** Rust 2021, tokio, ratatui 0.29, crossterm 0.28, rusqlite, serde_json. No new deps.

---

## File Structure

**Modified (heavily):**
- `src/app.rs` — replace `pty_tail` with `event_log: Vec<LogEntry>`; add `LogEntry` enum; add `claude_session_id` per session; add `pending_questions` map for non-blocking question pop-ups; add `OrcSessionView` so the orc tab uses the same `event_log` model.
- `src/main.rs` — drop `do_attach`, drop `pty_rx`/`pty_tx`, wire `WorkerRegistry`, register `WorkerEvent` channel, route worker events to `event_log`, route `Ctrl-C` per-tab as interrupt (only Ctrl-C from outside any session ever quits — quit is `q`-only), wire `R` to resume, wire review keys for view modes and editor handoff.
- `src/worker.rs` — add `interrupt()` method (sends `{"type":"interrupt"}` per stream-json control protocol, fallback to SIGINT on the child); add `resume(session_id)` constructor that uses `--resume` instead of seeding a fresh task; expose `claude_session_id` learned from the first `system` event.
- `src/state.rs` — `sweep_zombie_sessions` should no longer rely on tmux liveness; instead, on startup any session not present in the new in-memory `WorkerRegistry` is zombie. Add `StateCommand::AnswerOrc { session_id, answer }` so orc can answer via MCP. `UserQuestionPending` becomes idempotent (track outstanding question id, ignore second answer).
- `src/mcp.rs` — `ask_user` tool: dispatch to BOTH the user (state change) AND orc's stdin (forward) via a new injected `OrcInjector` handle. Add a counterpart so orc can call `answer_worker(session_id, answer)` to resolve the same question.
- `src/orc.rs` — add `inject_question(session_id, question)` that pushes a synthetic user message describing the worker's pending question; add a system-prompt clause explaining the answer protocol.
- `src/review.rs` — add `ViewMode` enum, `comments` accumulator keyed by `(file, line)`, ensure submit serialises both approvals and comments.
- `src/ui/mod.rs`, `src/ui/worker.rs` — single `render_event_log` function used by both orc and worker tabs. Drop dashboard import.
- `src/ui/modals.rs` — multi-line `NewTask` (Shift-Enter inserts `\n`); add `Comment { file, line, buffer }` modal; add `AskUser` "answered by orc" auto-dismiss display.
- `src/ui/review.rs` — add whole-file mode renderer; render comment markers in both views.
- `src/ui/tabs.rs` — show all six badges; ensure orc tab gets a badge too (always `◐` running).
- `src/db.rs` — add `claude_session_id TEXT` column to `sessions` (migration `m003`).
- `src/session.rs` — add `claude_session_id: Option<String>` field; add `interrupted: bool` transient flag for log purposes (no state-machine impact).

**Deleted:**
- `src/ui/dashboard.rs` — orc tab uses event-log render.
- `tmux::spawn_pane_capturer` callsite removed; capturer fn itself stays only if used by the `tst-` tmux test harness; otherwise delete it.

**Added:**
- `src/worker_registry.rs` — `WorkerRegistry` with `HashMap<String, WorkerHandle>`, async send/kill/interrupt/resume by session_id, plus `Drop` cleanup.

---

## Phase Layout & Sequencing

Phases are designed so each ends with a green `cargo build` and `cargo test`. Subagents may execute phases in this order. Phases 1-3 are sequential (each touches `app.rs`/`main.rs`); 4-6 can be parallelised after 3 lands.

1. **De-tmux workers + structured event log** (foundation).
2. **Unify orc tab as event-log peer** (UI normalisation).
3. **Pop-up input + per-tab interrupt** (input model).
4. **Worker→orc question forwarding** (orchestration loop).
5. **Three-mode review** (review ladder).
6. **Resume-on-restart** (continuity).
7. **Live verification** (mandatory per CLAUDE.md).

Each phase below has bite-sized steps.

---

## Phase 1 — De-tmux workers + structured event log

**Files:**
- Modify: `src/app.rs` — replace `pty_tail` with `event_log`, add `LogEntry`.
- Modify: `src/worker.rs` — already stream-json; add `interrupt()` method.
- Create: `src/worker_registry.rs`.
- Modify: `src/main.rs` — wire `WorkerRegistry`, route `WorkerEvent` into `event_log`, drop `pty_tx`/`pty_rx` and `tmux::spawn_pane_capturer`, drop `do_attach`.
- Modify: `src/state.rs` — `sweep_zombie_sessions` no longer queries tmux.
- Modify: `src/ui/worker.rs` — render `event_log` instead of `pty_tail`.
- Modify: `src/lib.rs` — export the new module if needed.

- [ ] **Step 1.1 — Add `LogEntry` enum and `event_log` field**

In `src/app.rs`, replace the `pty_tail: VecDeque<String>` field on `SessionView` with:

```rust
#[derive(Debug, Clone)]
pub enum LogEntry {
    UserText(String),
    AssistantText(String),
    Thinking(String),
    ToolUse { name: String, input_summary: String },
    ToolResult { text: String, is_error: bool },
    System(String), // [interrupted], answered by orc, etc.
    TurnEnd { cost_usd: Option<f64> },
}

#[derive(Debug, Clone)]
pub struct SessionView {
    pub session: Session,
    pub event_log: Vec<LogEntry>,
    pub permissions: Vec<PermissionEntry>,
    pub tab_index: usize,
    pub claude_session_id: Option<String>,
}
```

Replace `set_pty_tail`/`append_pty_line` with:

```rust
pub fn push_log(&mut self, session_id: &str, entry: LogEntry) {
    if let Some(&idx) = self.session_index.get(session_id) {
        self.sessions[idx].event_log.push(entry);
        // soft cap: keep last 5000 to avoid unbounded growth
        let log = &mut self.sessions[idx].event_log;
        if log.len() > 5000 {
            let drop = log.len() - 5000;
            log.drain(0..drop);
        }
    }
}

pub fn set_claude_session_id(&mut self, session_id: &str, sid: String) {
    if let Some(&idx) = self.session_index.get(session_id) {
        self.sessions[idx].claude_session_id = Some(sid);
    }
}
```

Update `add_session` to initialise `event_log: Vec::new()` and `claude_session_id: None`.

Update the `pty_tail_capacity` test in `src/app.rs:388-397` to test `event_log` cap instead — push 6000 `LogEntry::System("x".into())` and assert `event_log.len() == 5000`.

Run `cargo build` — expect failures in `main.rs`, `ui/worker.rs` referencing `pty_tail`. Those are fixed in next steps.

- [ ] **Step 1.2 — Create `WorkerRegistry`**

Create `src/worker_registry.rs`:

```rust
use crate::worker::{WorkerEvent, WorkerHandle};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, Default)]
pub struct WorkerRegistry {
    inner: Arc<Mutex<HashMap<String, WorkerHandle>>>,
}

impl WorkerRegistry {
    pub fn new() -> Self { Self::default() }

    pub async fn insert(&self, handle: WorkerHandle) {
        let id = handle.session_id.clone();
        self.inner.lock().await.insert(id, handle);
    }

    pub async fn send(&self, session_id: &str, msg: &str) -> Result<()> {
        let map = self.inner.lock().await;
        if let Some(h) = map.get(session_id) {
            h.send(msg).await?;
        }
        Ok(())
    }

    pub async fn interrupt(&self, session_id: &str) -> Result<()> {
        let map = self.inner.lock().await;
        if let Some(h) = map.get(session_id) {
            h.interrupt().await?;
        }
        Ok(())
    }

    pub async fn kill(&self, session_id: &str) -> Result<()> {
        let mut map = self.inner.lock().await;
        if let Some(h) = map.remove(session_id) {
            h.kill().await?;
        }
        Ok(())
    }

    pub async fn contains(&self, session_id: &str) -> bool {
        self.inner.lock().await.contains_key(session_id)
    }

    pub async fn ids(&self) -> Vec<String> {
        self.inner.lock().await.keys().cloned().collect()
    }
}
```

Add `pub mod worker_registry;` to `src/lib.rs` and `src/main.rs`. Run `cargo build` — should compile this module on its own.

- [ ] **Step 1.3 — Add `WorkerHandle::interrupt`**

In `src/worker.rs`, add:

```rust
impl WorkerHandle {
    pub async fn interrupt(&self) -> Result<()> {
        // Stream-json control: send a synthetic user message that says STOP.
        // Claude's stream-json input doesn't currently honour an out-of-band
        // interrupt, so we send SIGINT to the child to cancel the in-flight turn.
        // The conversation stays alive because we don't kill the process.
        let child = self.child.lock().await;
        if let Some(pid) = child.id() {
            // SIGINT — tokio::process::Child::id returns u32 pid on unix.
            unsafe {
                libc::kill(pid as i32, libc::SIGINT);
            }
        }
        Ok(())
    }
}
```

Add `libc = "0.2"` to `Cargo.toml` under `[dependencies]`.

Run `cargo build` and confirm.

- [ ] **Step 1.4 — Drop tmux from main loop and route WorkerEvents**

In `src/main.rs`:
- Remove `pty_tx`/`pty_rx` channel and the `pty_rx.recv()` arm of the `select!`.
- Remove the `tmux::spawn_pane_capturer(...)` call inside `StateChange::SessionCreated`.
- Remove the `do_attach` function and the `KeyAction::Attach` variant; remove the `Enter` keybind that triggered it.
- Remove the final `tmux::cleanup_orphaned()` call at shutdown — replace with `worker_registry.kill_all().await`.
- Add a `WorkerRegistry` and a `(WorkerEvent) mpsc::UnboundedReceiver`. Add a new `select!` arm `Some(ev) = worker_rx.recv()` that maps `WorkerEvent::Text` → `LogEntry::AssistantText`, `Thinking` → `Thinking`, `ToolUse{name,input}` → `ToolUse{name, input_summary: truncate_json(&input)}`, `ToolResult{text,is_error}` → `ToolResult{text,is_error}`, `Result{cost_usd,..}` → `TurnEnd{cost_usd}`, `Exited{code}` → push `LogEntry::System(format!("worker exited code {code:?}"))` and call `worker_registry.kill(&session_id)` to drop the handle.

Add a `kill_all` helper to `WorkerRegistry`:

```rust
pub async fn kill_all(&self) {
    let mut map = self.inner.lock().await;
    for (_, h) in map.drain() {
        let _ = h.kill().await;
    }
}
```

Pass `WorkerRegistry` and `worker_tx` into the spawn pipeline used by `mcp.rs::spawn_worker_session` (or wherever workers are spawned today — see `src/mcp.rs` for `spawn_worker` callsites). Replace `tmux::create_session(...)` and the `claude -p ... | tmux send-keys` shape with `worker::spawn_worker(...)` and `worker_registry.insert(handle).await`.

Run `cargo build`. Fix every reference to `tmux_session` outside `db.rs` and tests by removing it. Inside `state.rs::sweep_zombie_sessions`, replace `tmux::has_session(&s.tmux_session).await` with: on startup, mark every Running/AwaitingReview session as Failed (zombie sweep no longer infers from tmux — workers don't survive an orc restart in v2).

- [ ] **Step 1.5 — Render `event_log` in worker tab**

In `src/ui/worker.rs`, replace `render_pty_tail` with `render_event_log` that walks `sv.event_log` and renders each entry as a `Line`:

```rust
fn render_event_log(frame: &mut Frame, area: Rect, sv: &SessionView, scroll: usize) {
    use ratatui::style::{Color, Modifier, Style};
    let mut lines: Vec<Line> = Vec::with_capacity(sv.event_log.len());
    for entry in &sv.event_log {
        let line = match entry {
            LogEntry::UserText(t) => Line::from(vec![
                Span::styled("you  ", Style::default().fg(Color::Cyan)),
                Span::raw(t.clone()),
            ]),
            LogEntry::AssistantText(t) => Line::from(Span::raw(t.clone())),
            LogEntry::Thinking(t) => Line::from(Span::styled(
                format!("(thinking) {t}"),
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            )),
            LogEntry::ToolUse { name, input_summary } => Line::from(vec![
                Span::styled("→ ", Style::default().fg(Color::Yellow)),
                Span::raw(format!("{name}({input_summary})")),
            ]),
            LogEntry::ToolResult { text, is_error } => {
                let color = if *is_error { Color::Red } else { Color::Green };
                Line::from(vec![
                    Span::styled("← ", Style::default().fg(color)),
                    Span::raw(if text.len() > 200 { format!("{}…", &text[..200]) } else { text.clone() }),
                ])
            }
            LogEntry::System(s) => Line::from(Span::styled(
                format!("[{s}]"),
                Style::default().fg(Color::DarkGray),
            )),
            LogEntry::TurnEnd { cost_usd } => Line::from(Span::styled(
                cost_usd.map(|c| format!("— turn end (${c:.4}) —")).unwrap_or_else(|| "— turn end —".into()),
                Style::default().fg(Color::DarkGray),
            )),
        };
        lines.push(line);
    }
    let para = Paragraph::new(lines).scroll((scroll as u16, 0));
    frame.render_widget(para, area);
}
```

Replace the `render_pty_tail` callsite in `render_worker` with `render_event_log(frame, area, sv, app.scroll_pos(...))`. `render_worker` will need the scroll position passed in — add it to the signature and the callers in `ui/mod.rs`.

Delete `render_pty_tail`, `set_pty_tail`, `append_pty_line` everywhere they remain.

Run `cargo build`. Run `cargo test`. Fix tests touching `pty_tail` or `tmux_session` to use `event_log` and a placeholder string respectively.

- [ ] **Step 1.6 — Commit Phase 1**

```bash
git add -A
git commit -m "feat: workers as stream-json children, event_log replaces pty tail"
```

---

## Phase 2 — Unify orc tab as event-log peer

**Files:**
- Modify: `src/app.rs` — replace `orc_chat: Vec<ChatMessage>` with an `OrcView` that holds `event_log: Vec<LogEntry>` (same enum). Keep `push_chat` as a thin wrapper that pushes a `LogEntry::UserText` / `LogEntry::AssistantText` / `LogEntry::System` so existing callsites compile.
- Delete: `src/ui/dashboard.rs`.
- Modify: `src/ui/mod.rs` — call `render_event_log` for both `TabId::Orc` (passing `OrcView.event_log`) and `TabId::Worker(idx)` (passing `SessionView.event_log`). Remove `dashboard` module.
- Modify: `src/main.rs::handle_orc_event` — push `LogEntry::AssistantText` / `LogEntry::ToolUse` / `LogEntry::TurnEnd` / `LogEntry::System` to `app.orc_view.event_log` instead of the chat vec.
- Modify: `src/ui/tabs.rs` — orc tab gets a badge (`◐` while running, `✗` if orc process died).

- [ ] **Step 2.1 — Replace `orc_chat` with `orc_view: OrcView`**

In `src/app.rs`:

```rust
pub struct OrcView {
    pub event_log: Vec<LogEntry>,
    pub alive: bool,
}

impl OrcView {
    pub fn new() -> Self {
        Self { event_log: Vec::new(), alive: true }
    }
    pub fn push(&mut self, entry: LogEntry) {
        self.event_log.push(entry);
        if self.event_log.len() > 5000 {
            let drop = self.event_log.len() - 5000;
            self.event_log.drain(0..drop);
        }
    }
}
```

Replace `orc_chat: Vec<ChatMessage>` with `orc_view: OrcView`. Keep a temporary `push_chat` shim:

```rust
pub fn push_chat(&mut self, role: ChatRole, text: String) {
    let entry = match role {
        ChatRole::User => LogEntry::UserText(text),
        ChatRole::Orc => LogEntry::AssistantText(text),
        ChatRole::System => LogEntry::System(text),
    };
    self.orc_view.push(entry);
}
```

This lets every existing callsite keep working until cleaned up. Delete `ChatMessage` only after cleanups; for now keep it (it's only used in tests).

- [ ] **Step 2.2 — Delete dashboard, route orc to `render_event_log`**

Delete `src/ui/dashboard.rs`. Remove `pub mod dashboard;` from `src/ui/mod.rs`.

In `src/ui/mod.rs::render`:

```rust
match app.focused_tab {
    TabId::Orc => {
        render_orc_tab(frame, layout[1], app);
    }
    TabId::Worker(idx) => {
        if let Some(sv) = app.sessions.get(idx) {
            render_worker_tab(frame, layout[1], sv, app.scroll_pos(TabId::Worker(idx)));
        }
    }
}
```

Implement `render_orc_tab` in `src/ui/mod.rs` (small enough to live there): a header line ("orc · model · alive/dead") then a paragraph that walks `app.orc_view.event_log` with the same per-entry rendering function from `worker.rs`. Pull that rendering out into a free function `pub fn log_lines(log: &[LogEntry]) -> Vec<Line<'static>>` in `src/ui/worker.rs` and use it from both call sites.

- [ ] **Step 2.3 — Orc tab badge in tab strip**

In `src/ui/tabs.rs`, at the orc-tab rendering site, badge with `◐` if `app.orc_view.alive`, else `✗`. Set `alive = false` in `main.rs` when `orc_process.is_alive()` returns false during `read_events`.

- [ ] **Step 2.4 — Build & test & commit**

`cargo build && cargo test`. Fix test breakage (`render_does_not_panic_*`, `chat_push`).

```bash
git add -A
git commit -m "feat: orc tab uses unified event-log renderer"
```

---

## Phase 3 — Pop-up input + per-tab interrupt

**Files:**
- Modify: `src/main.rs` — `n` opens a `Modal::NewTask { target: TabId, buffer: String }`, multi-line via Shift-Enter, sends to orc OR to focused worker depending on `target`. `Ctrl-C` interrupts the focused tab (calls `orc.interrupt()` for orc tab, `worker_registry.interrupt(id)` for a worker tab).
- Modify: `src/app.rs` — `Modal::NewTask` gains `target: TabId`.
- Modify: `src/ui/modals.rs` — multi-line text rendering, hint shows "Enter send · Shift-Enter newline · Esc cancel".
- Modify: `src/orc.rs` — add `interrupt()` analogous to worker's.

- [ ] **Step 3.1 — `Modal::NewTask` gains `target`**

In `src/app.rs`:

```rust
pub enum Modal {
    NewTask { target: TabId, buffer: String },
    AskUser { /* unchanged */ },
    Comment { session_id: String, file: String, line: usize, buffer: String },
    ConfirmKill { session_id: String, name: String },
    ConfirmQuit,
    Help,
}
```

- [ ] **Step 3.2 — `n` opens modal targeting focused tab**

In `src/main.rs::handle_key`:

```rust
KeyCode::Char('n') => {
    app.modal = Some(Modal::NewTask { target: app.focused_tab, buffer: String::new() });
}
```

In `handle_modal_key`, `Modal::NewTask` Enter handler:

```rust
KeyCode::Enter if !buffer.is_empty() && !key.modifiers.contains(KeyModifiers::SHIFT) => {
    match target {
        TabId::Orc => return KeyAction::SendToOrc(buffer),
        TabId::Worker(idx) => {
            if let Some(sv) = app.sessions.get(idx) {
                let id = sv.session.id.clone();
                let body = buffer.clone();
                app.push_log(&id, LogEntry::UserText(body.clone()));
                return KeyAction::SendToWorker { session_id: id, body };
            }
        }
    }
}
KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
    buffer.push('\n');
    app.modal = Some(Modal::NewTask { target, buffer });
}
```

Add the new `KeyAction::SendToWorker { session_id: String, body: String }` variant. Handle it in the event loop with `worker_registry.send(&session_id, &body).await`.

- [ ] **Step 3.3 — Multi-line modal rendering**

In `src/ui/modals.rs::render_new_task`, render `buffer` as a `Paragraph` with `wrap: trim=false` so newlines are honored. Update the modal title to "speak to <orc|worker name>" depending on `target`. Update hint to "Enter send · Shift-Enter newline · Esc cancel".

- [ ] **Step 3.4 — Per-tab Ctrl-C as interrupt**

In `src/main.rs::handle_key`, replace the existing global Ctrl-C-quit:

```rust
if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
    match app.focused_tab {
        TabId::Orc => return KeyAction::InterruptOrc,
        TabId::Worker(idx) => {
            if let Some(sv) = app.sessions.get(idx) {
                return KeyAction::InterruptWorker(sv.session.id.clone());
            }
        }
    }
    return KeyAction::None;
}
```

Add `KeyAction::InterruptOrc` and `KeyAction::InterruptWorker(String)`. In the event loop, dispatch them to `orc_process.interrupt().await` and `worker_registry.interrupt(&id).await`, and push a `LogEntry::System("interrupted".into())` to the relevant log. Quit becomes `q` only.

- [ ] **Step 3.5 — `OrcProcess::interrupt`**

In `src/orc.rs`:

```rust
pub async fn interrupt(&mut self) -> Result<()> {
    if let Some(child) = self.child.as_mut() {
        if let Some(pid) = child.id() {
            unsafe { libc::kill(pid as i32, libc::SIGINT); }
        }
    }
    Ok(())
}
```

(Adapt the field path to whatever `OrcProcess` actually holds — check `src/orc.rs`.)

- [ ] **Step 3.6 — Build, test, live-verify, commit**

`cargo build && cargo test`. Then live verify per CLAUDE.md harness:

```bash
TMUX= tmux new-session -d -s tst-input -x 140 -y 50 \
  "TMUX= cargo run 2>/tmp/orc-stderr.log; echo EXIT=\$? > /tmp/orc-exit.log; sleep 60"
sleep 8
TMUX= tmux send-keys -t tst-input -l 'n'
sleep 1
TMUX= tmux send-keys -t tst-input -l 'hello orc'
sleep 1
TMUX= tmux send-keys -t tst-input Enter
sleep 2
TMUX= tmux capture-pane -t tst-input -p | head -50
TMUX= tmux send-keys -t tst-input -l 'q'
sleep 1
TMUX= tmux send-keys -t tst-input -l 'y'
sleep 2
cat /tmp/orc-exit.log
TMUX= tmux kill-session -t tst-input 2>/dev/null
```

Expect: `n` opens modal, typing "hello orc" + Enter sends to orc and the message appears as a `you` line in the orc tab's event log, `q`+`y` exits with `EXIT=0`.

```bash
git add -A
git commit -m "feat: pop-up input model, per-tab Ctrl-C interrupt"
```

---

## Phase 4 — Worker→orc question forwarding

**Files:**
- Modify: `src/mcp.rs::ask_user` tool — when called by a worker, push a `StateChange::UserQuestionPending` AND push a synthetic message into orc's stdin so orc sees the question.
- Modify: `src/orc.rs` — add `inject_question(question_id, session_id, question)` method that writes a `{"type":"user","message":...}` line describing the worker's question. The orc system prompt already names workers; extend it to explain that orc may answer worker questions by calling the `answer_worker` MCP tool.
- Modify: `src/mcp.rs` — add `answer_worker(question_id, answer)` MCP tool. Resolves the same outstanding question via `state_handle.answer_user`.
- Modify: `src/state.rs` — `StateManager::answer_user` becomes idempotent: stores outstanding question id; second answer is a no-op. Emits `StateChange::QuestionResolved { session_id, answered_by }` so UI can dismiss the modal regardless of who answered.
- Modify: `src/main.rs` — handle `QuestionResolved` to dismiss the modal and push `LogEntry::System(format!("answered by {answered_by}: ..."))` into the asking session's log.

- [ ] **Step 4.1 — Track outstanding question id**

In `src/state.rs::StateManager`, add `outstanding_questions: HashMap<String /*session_id*/, String /*question_id*/>`. When `ask_user` lands, generate a uuid and store. When `answer_user` is called, only act if the supplied or current outstanding id matches; clear it; emit `QuestionResolved`.

```rust
StateChange::QuestionResolved { session_id: String, question_id: String, answered_by: AnsweredBy }

pub enum AnsweredBy { User, Orc }
```

Update `StateChange` and the `answer_user` path to emit it. Also widen the `UserQuestionPending` event to include `question_id`.

- [ ] **Step 4.2 — Inject question into orc**

In `src/orc.rs`:

```rust
pub async fn inject_question(&mut self, session_id: &str, question_id: &str, question: &str) -> Result<()> {
    let payload = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": format!(
                "[worker {session_id} asked, question_id={question_id}]: {question}\n\
                 You may answer it by calling the `answer_worker` MCP tool with the question_id and your answer. \
                 If you don't have enough context, stay quiet — the user will be prompted."
            )
        }
    });
    self.send_raw(&payload.to_string()).await
}
```

(`send_raw` already exists as the underlying NDJSON write — if not, add it.)

In the system prompt (`src/orc.rs::system_prompt`), append a paragraph explaining the protocol: workers ask via `ask_user`, orc may answer via `answer_worker`, first responder wins.

- [ ] **Step 4.3 — `answer_worker` MCP tool**

In `src/mcp.rs`, add a tool:

```rust
"answer_worker" => {
    let question_id = args["question_id"].as_str().unwrap_or("");
    let answer = args["answer"].as_str().unwrap_or("");
    state_handle.answer_user_with_id(question_id, answer, AnsweredBy::Orc).await?;
    json!({ "ok": true })
}
```

Add a corresponding `answer_user_with_id` to `StateHandle` that targets the question_id explicitly.

- [ ] **Step 4.4 — Wire forwarding on `ask_user`**

In `src/mcp.rs::ask_user`, after enqueuing the `UserQuestionPending` change, call `orc_injector.inject_question(session_id, question_id, question)`. Pass an `OrcInjector` (a thin wrapper over `Arc<Mutex<OrcProcess stdin>>`) into `McpServer::new` from `main.rs`.

- [ ] **Step 4.5 — Handle `QuestionResolved` in UI**

In `src/main.rs::handle_state_change`:

```rust
StateChange::QuestionResolved { session_id, question_id, answered_by } => {
    // Dismiss the AskUser modal if it matches this question.
    if let Some(Modal::AskUser { question_id: open_id, .. }) = &app.modal {
        if open_id == &question_id { app.modal = None; }
    }
    let by = match answered_by { AnsweredBy::User => "you", AnsweredBy::Orc => "orc" };
    app.push_log(&session_id, LogEntry::System(format!("answered by {by}")));
}
```

Add `question_id: String` to `Modal::AskUser`.

- [ ] **Step 4.6 — Build, test, commit**

`cargo build && cargo test`.

```bash
git add -A
git commit -m "feat: worker questions forward to orc; first-responder wins"
```

---

## Phase 5 — Three-mode review

**Files:**
- Modify: `src/review.rs` — add `ViewMode { Diff, WholeFile }`, `comments: BTreeMap<(String, usize), String>`, ensure `to_payload` includes both approvals and comments.
- Modify: `src/main.rs::handle_review_key` — `o` toggles mode; `e` shells out to `$EDITOR`.
- Modify: `src/ui/review.rs` — render whole-file mode; show comment markers in both views.
- Modify: `src/ui/modals.rs` — proper `Modal::Comment` modal (use existing pattern), removing the TODO at `src/main.rs:585`.

- [ ] **Step 5.1 — `ViewMode` and comments**

In `src/review.rs`, add fields and impl:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode { Diff, WholeFile }

pub struct ReviewState {
    /* existing fields */
    pub view_mode: ViewMode,
    pub comments: std::collections::BTreeMap<(String /*file*/, usize /*line*/), String>,
}

impl ReviewState {
    pub fn add_comment(&mut self, file: String, line: usize, body: String) {
        self.comments.insert((file, line), body);
    }
    pub fn toggle_view(&mut self) {
        self.view_mode = match self.view_mode {
            ViewMode::Diff => ViewMode::WholeFile,
            ViewMode::WholeFile => ViewMode::Diff,
        };
    }
}
```

Update `to_payload` so the JSON sent to the worker includes both per-hunk approvals and a `comments: [{ file, line, body }, ...]` array.

- [ ] **Step 5.2 — `Modal::Comment`**

In `src/app.rs::Modal`, add `Comment { session_id: String, file: String, line: usize, buffer: String }`. In `src/ui/modals.rs`, render it analogously to `NewTask`. In `src/main.rs::handle_modal_key`, on Enter add the comment to `app.review.as_mut().add_comment(...)` and dismiss.

In `src/main.rs::handle_review_key`, replace the TODO at line 585:

```rust
KeyCode::Char('c') => {
    if let Some(file) = review.current_file() {
        let line = review.current_line().and_then(|l| l.new_lineno).unwrap_or(0);
        app.modal = Some(Modal::Comment {
            session_id: review.session_id.clone(),
            file: file.into(),
            line: line as usize,
            buffer: String::new(),
        });
    }
}
```

- [ ] **Step 5.3 — `o` toggles view mode**

In `handle_review_key`:

```rust
KeyCode::Char('o') => review.toggle_view(),
```

In `src/ui/review.rs::render_review`, branch on `review.view_mode`:

```rust
match review.view_mode {
    ViewMode::Diff => render_diff_view(frame, area, review),
    ViewMode::WholeFile => render_whole_file_view(frame, area, review),
}
```

Implement `render_whole_file_view`: for `review.current_file()`, read the file content from the worker's worktree via `std::fs::read_to_string`, render with line numbers, overlay comment markers (`💬` glyph) where `review.comments` has an entry for that file+line. (No emoji unless user requested — use the literal character `*` instead.)

- [ ] **Step 5.4 — `e` shells to `$EDITOR`**

In `handle_review_key`:

```rust
KeyCode::Char('e') => {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let target = review.current_file_abs_path(); // helper on ReviewState
    return KeyAction::Editor { command: editor, target };
}
```

In the event loop, on `KeyAction::Editor`: leave alt-screen, run the editor as a blocking child, re-enter alt-screen, redraw. Mirror the disable-raw-mode dance currently done in `do_attach`.

- [ ] **Step 5.5 — Build, test, commit**

`cargo build && cargo test`.

```bash
git add -A
git commit -m "feat: review three-mode ladder (diff/whole-file/editor)"
```

---

## Phase 6 — Resume on restart

**Files:**
- Modify: `src/db.rs` — add `claude_session_id TEXT` column. Add migration `m003__claude_session_id.sql`.
- Modify: `src/session.rs` — `claude_session_id: Option<String>`.
- Modify: `src/worker.rs` — capture the `system` event's `session_id` and emit a new `WorkerEvent::ClaudeSessionId { session_id, claude_session_id }`. Add a `spawn_worker_resume(claude_session_id, ...)` constructor that uses `--resume <id>` instead of seeding the task.
- Modify: `src/main.rs` — on `WorkerEvent::ClaudeSessionId`, persist via `state_handle.set_claude_session_id(session_id, claude_session_id).await`. The `R` key path: don't recreate the worktree; call `worker::spawn_worker_resume(...)` and `worker_registry.insert(...)`.

- [ ] **Step 6.1 — Persist `claude_session_id`**

Add migration. In `src/db.rs::Database::open`, run the `ALTER TABLE sessions ADD COLUMN claude_session_id TEXT` if absent, behind a guard. Update `insert_session`, `select_session`, and the `Session` row mapping. Add `update_claude_session_id(id, sid)`.

In `src/session.rs::Session`, add `pub claude_session_id: Option<String>`. Default to `None` in constructors.

In `src/state.rs`, add `StateCommand::SetClaudeSessionId { session_id, claude_session_id }` and a corresponding `StateHandle::set_claude_session_id` async helper.

- [ ] **Step 6.2 — Capture from `system` event**

In `src/worker.rs::parse_worker_events`, add a `system` arm:

```rust
"system" => {
    if let Some(sid) = raw.get("session_id").and_then(|v| v.as_str()) {
        out.push(WorkerEvent::ClaudeSessionId {
            session_id: session_id.to_string(),
            claude_session_id: sid.to_string(),
        });
    }
}
```

Add the variant to `WorkerEvent`.

In `main.rs`, on `WorkerEvent::ClaudeSessionId`, call `state_handle.set_claude_session_id(...).await` and `app.set_claude_session_id(...)`.

- [ ] **Step 6.3 — `spawn_worker_resume`**

In `src/worker.rs`, factor the spawn body so it can build the `Command` either with `-p` + initial task or with `--resume <id>` and no initial send:

```rust
pub async fn spawn_worker_resume(
    session_id: String,
    worktree: PathBuf,
    model: String,
    mcp_config_path: PathBuf,
    system_prompt: String,
    claude_session_id: String,
    events_tx: mpsc::UnboundedSender<WorkerEvent>,
) -> Result<WorkerHandle> { /* identical except: --resume <id>, no send(task) */ }
```

- [ ] **Step 6.4 — `R` resumes**

In `src/main.rs::handle_key`'s `R` arm, if `sv.session.claude_session_id` is `Some`, call `spawn_worker_resume`, register, and transition state Failed→Running via `state_handle.send(StateCommand::ApplyEvent { event: SessionEvent::Restarted, ... })`. If `claude_session_id` is `None` (worker died before first turn), fall back to the existing fresh-spawn path.

- [ ] **Step 6.5 — Build, test, commit**

```bash
git add -A
git commit -m "feat: R restarts worker with claude --resume continuity"
```

---

## Phase 7 — Live verification

Per CLAUDE.md, drive the binary and verify each new flow.

- [ ] **Step 7.1 — Cleanup environment**

```bash
pkill -f "^orc$"; pkill -f "claude -p" || true
rm -f ~/.config/orc/state.db ~/.config/orc/state.db-shm ~/.config/orc/state.db-wal
git worktree prune
ls ../.orc-worktrees/ 2>/dev/null && rm -rf ../.orc-worktrees/* || true
```

- [ ] **Step 7.2 — Startup renders cleanly**

```bash
TMUX= tmux new-session -d -s tst-startup -x 140 -y 50 \
  "TMUX= cargo run 2>/tmp/orc-stderr.log; echo EXIT=\$? > /tmp/orc-exit.log; sleep 60"
sleep 8
TMUX= tmux capture-pane -t tst-startup -p > /tmp/startup.txt
grep -q "orc" /tmp/startup.txt
TMUX= tmux send-keys -t tst-startup -l 'q'; sleep 1
TMUX= tmux send-keys -t tst-startup -l 'y'; sleep 2
cat /tmp/orc-exit.log
TMUX= tmux kill-session -t tst-startup 2>/dev/null || true
```

Expect `EXIT=0`, no orphans (`pgrep -lf orc` empty after exit).

- [ ] **Step 7.3 — Worker spawn end-to-end**

```bash
TMUX= tmux new-session -d -s tst-spawn -x 140 -y 50 \
  "TMUX= cargo run 2>/tmp/orc-stderr.log; echo EXIT=\$? > /tmp/orc-exit.log; sleep 240"
sleep 8
# Ask orc to spawn a tiny worker
TMUX= tmux send-keys -t tst-spawn -l 'n'; sleep 1
TMUX= tmux send-keys -t tst-spawn -l 'spawn one worker named probe and have it run "echo hi" via Bash'
sleep 1
TMUX= tmux send-keys -t tst-spawn Enter
sleep 60   # let orc plan + spawn + run
TMUX= tmux capture-pane -t tst-spawn -p > /tmp/spawn.txt
TMUX= tmux send-keys -t tst-spawn Tab; sleep 1   # focus worker tab
TMUX= tmux capture-pane -t tst-spawn -p > /tmp/worker.txt
# Cleanup
TMUX= tmux send-keys -t tst-spawn -l 'q'; sleep 1
TMUX= tmux send-keys -t tst-spawn -l 'y'; sleep 3
cat /tmp/orc-exit.log
sqlite3 ~/.config/orc/state.db "select name, state from sessions" || true
TMUX= tmux kill-session -t tst-spawn 2>/dev/null || true
```

Expect: worker tab shows event log entries (assistant text, ToolUse, ToolResult); state moves Running → Done; `EXIT=0`; no leftover claude processes.

- [ ] **Step 7.4 — Doctor**

```bash
cargo run -- doctor
```

Expect all checks pass, no orphans flagged. (Doctor still checks tmux because tmux remains a test harness dep — that's fine.)

- [ ] **Step 7.5 — Commit verification artefacts**

```bash
git add -A
git commit -m "test: live-verified spec compliance end-to-end"
```

---

## Self-review

- Spec coverage: ✓ tabs (Phase 2 unifies orc with worker tabs); ✓ structured event log (Phase 1); ✓ pop-up input (Phase 3); ✓ questions forwarded to orc (Phase 4); ✓ first-responder (Phase 4 idempotency); ✓ interrupt per tab (Phase 3); ✓ kill (already present, kept); ✓ restart with continuity (Phase 6); ✓ review three modes (Phase 5); ✓ no attach-into-claude (Phase 1 deletion); ✓ no PTY tail (Phase 1 deletion); ✓ tmux removed from worker arch (Phase 1).
- Placeholder scan: no TBD/TODO; comment-modal TODO at `main.rs:585` is removed in Phase 5.2.
- Type consistency: `LogEntry`, `WorkerRegistry`, `Modal::NewTask::target`, `KeyAction::SendToWorker`, `Modal::Comment`, `ReviewState::view_mode`, `WorkerEvent::ClaudeSessionId` — all defined once and reused.
