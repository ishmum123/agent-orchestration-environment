# Orc UI — ASCII snapshots

Captured live from `cargo run` in a 140x50 tmux pane on 2026-05-09. Snapshots
trimmed and re-rendered for readability; layout invariants preserved.

## 1. Base view — orc tab, idle

```
 ◐ orc
┌ events ─────────────────────────────────────────────────┐┌──────────────┐
│[orc v2 — MCP on port 55136]                             ││▌ ◐ orc       │
│                                                         ││▌   orchestr. │
│                                                         ││▌             │
│                                                         ││▌  alive · 8s │
│                                                         │└──────────────┘
│                  (event log — empty until orc speaks)   │
│                                                         │
└─────────────────────────────────────────────────────────┘
 n speak   1-9 tab   ^C interrupt   q quit   ? help
```

Header is the focused-entity line (` ◐ orc`). Right rail is the agents panel —
gutter glyph + name + role + state/elapsed. Bottom action bar is
context-aware.

## 2. Speak modal (centered pop-up, scoped to focused tab)

```
 ◐ orc
┌ events ─────────────────────────────────────────────────┐┌──────────────┐
│[orc v2 — MCP on port 55136]                             ││▌ ◐ orc       │
│           ┌ speak to orc ──────────────────────────────┐││▌   orchestr. │
│           │hey I am orc — give me a task or list of    │││▌             │
│           │tasks and I'll help you complete them       │││▌  alive · …  │
│           │                                            │└──────────────┘
│           │describe the task for orc to plan:          │
│           │┌──────────────────────────────────────────┐│
│           ││_                                         ││
│           ││                                          ││
│           ││  (multiline buffer, grows up to a cap)   ││
│           │└──────────────────────────────────────────┘│
│           │enter send   shift+enter newline   esc cancel│
│           └────────────────────────────────────────────┘
└─────────────────────────────────────────────────────────┘
```

Same modal shape for "speak to <worker>" — only title and helper text change.

## 3. Help overlay (`?`)

```
 ◐ orc
┌ events ─────────────────────────────────────────────────┐
│           ┌ keybindings ───────────────────────────────┐│
│           │tab / shift+tab     cycle tabs              ││
│           │1-9                 jump to tab             ││
│           │n                   new task                ││
│           │k                   kill focused worker     ││
│           │q                   quit                    ││
│           │?                   toggle help             ││
│           │j / ↓               scroll down             ││
│           │k / ↑               scroll up               ││
│           │g / G               top / bottom            ││
│           │enter               send chat input         ││
│           │esc                 close modal / deselect  ││
│           └────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────┘
```

## 4. Worker tab — running, with event log

```
 ◐ orc   ◐ w1·refactor-api   ! w2·flaky-test
┌ events ─────────────────────────────────────────────────┐┌──────────────┐
│assistant: I'll start by mapping the handlers under …    ││▌ ◐ orc       │
│→ Read({ "path": "src/api/mod.rs" })                     ││▌   orchestr. │
│← ok                                                     ││▌             │
│→ Grep({ "pattern": "fn handle_" })                      ││▌ ◐ w1·refac… │
│← ok                                                     ││▌   worker    │
│assistant: Three handlers share the auth preamble …      ││▌  running·2m │
│[answered by orc: yes, factor it out]                    ││▌             │
│→ Edit({ "path": "src/api/mod.rs", … })                  ││▌ ! w2·flaky… │
│← ok                                                     ││▌   blocked   │
│                                                         │└──────────────┘
└─────────────────────────────────────────────────────────┘
 n speak   r review   k kill   R restart   ^C interrupt   1-9 tab   q quit
```

Tab strip badges: `◐` running · `!` blocked on question · `?` blocked on
permission · `◑` awaiting review · `✓` done · `✗` failed. Self-MCP tool
calls are filtered from the log.

## 5. Question pop-up (worker raised; you or orc may answer)

```
 ◐ orc   ! w2·flaky-test
┌ events ─────────────────────────────────────────────────┐
│         ┌ w2 asks ───────────────────────────────────┐  │
│         │The retry budget is ambiguous in the spec.  │  │
│         │Should I treat 3 attempts as inclusive of   │  │
│         │the first call, or 3 retries after it?      │  │
│         │                                            │  │
│         │┌──────────────────────────────────────────┐│  │
│         ││_                                         ││  │
│         │└──────────────────────────────────────────┘│  │
│         │enter send   esc dismiss (orc may answer)   │  │
│         └────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
```

Non-blocking — `Tab` away freely; the asking tab's badge keeps flashing
until resolved.

## 6. Review — diff view (`r`)

```
 ◑ w1·refactor-api
┌ files ──────────┐┌ diff: src/api/mod.rs ──────────────────┐
│ M src/api/mod.rs││  12  fn handle_login(req: Req) -> Resp {│
│ M src/api/auth  ││- 13      let ctx = auth::check(&req)?;  │
│ A src/api/util  ││+ 13      let ctx = preamble(&req)?;     │
│                 ││  14      …                              │
│                 ││  15  }                                  │
│                 ││                                         │
│                 ││  ▌ comment here (c)                     │
└─────────────────┘└─────────────────────────────────────────┘
 c comment   a approve   j/k navigate   o whole-file   e editor   s submit   q cancel
```

Ladder: `r` diff → `o` whole-file → `e` `$EDITOR`. Comments and approvals
share one review session across views.

## 7. Kill confirmation

```
              ┌ kill w2·flaky-test? ──────────────┐
              │This terminates the worker, drops  │
              │its tab and worktree.              │
              │                                   │
              │   y confirm        n / esc cancel │
              └───────────────────────────────────┘
```

## Layout invariants

- One-line focused-entity header at the top of every tab.
- Scrollable event log (or split-pane in review) as the main body.
- Agents panel on the right rail across worker-focused tabs.
- Action bar re-keys to the current context.
