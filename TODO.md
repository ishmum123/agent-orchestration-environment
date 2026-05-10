# TODO

- **Claude shim for tests.** Live-verifying TUI/UX changes today spawns real `claude` children and burns tokens. Build a fake-claude binary (or env-switched stub) that speaks the stream-json protocol so harness runs cost nothing and are deterministic. Wire `CLAUDE.md`'s harness section to use it by default.

- **Architecture rewrite around data models.** Current code grew organically — `Session`, `SessionView`, `OrcView`, `OrcUsage`, `App`, `StateManager` overlap and duplicate fields (e.g. context tokens now live in two places, summaries in a sidecar `HashMap`). Re-design top-down from the data: one canonical model per concept, projections derived, no parallel structs drifting.

- **Auto-start task when usage limit reached.** When the user's Claude plan hits a usage cap mid-run, workers stall. Detect the limit (claude returns a recognizable error in stream-json), pause the affected session, and resume automatically when the quota window resets — no manual restart.

- **Compact UI outputs much more.** Worker tab event log is verbose: tool calls and results take many lines each, exploration rollups help but the per-event spacing is still loose. Tighten line counts, collapse repetitive tool sequences more aggressively, drop blank separators where they don't aid scanning.
