# TODO

- **Architecture rewrite around data models.** Current code grew organically — `Session`, `SessionView`, `OrcView`, `OrcUsage`, `App`, `StateManager` overlap and duplicate fields (e.g. context tokens now live in two places, summaries in a sidecar `HashMap`). Re-design top-down from the data: one canonical model per concept, projections derived, no parallel structs drifting.

- **Auto-start task when usage limit reached.** When the user's Claude plan hits a usage cap mid-run, workers stall. Detect the limit (claude returns a recognizable error in stream-json), pause the affected session, and resume automatically when the quota window resets — no manual restart.

- **Compact UI outputs much more.** Worker tab event log is verbose: tool calls and results take many lines each, exploration rollups help but the per-event spacing is still loose. Tighten line counts, collapse repetitive tool sequences more aggressively, drop blank separators where they don't aid scanning.

- **Restyle worker tabs and orc tab to match the scratch overlay aesthetic.** The new `?` overlay's chat layout (clean `you/claude` blocks, generous spacing, dim metadata, tight action bar, rotating `◐ ◓ ◑ ◒` "thinking" spinner) reads better than the current worker tab event-log. Apply the same vocabulary to worker tabs and the orc tab: same header treatment, same conversation block style, same hint bar, same spinner/animation cadence for in-flight turns. Factor the chat-block renderer and the spinner so all three views share them.

- **Panel-scoped mouse selection.** Mouse selection currently spans the whole terminal row, mixing content from different panels (e.g. event log and right-side agents panel). Make selection respect panel boundaries: dragging inside the input box selects only input box text; dragging in the events area selects only events; the agents panel selects only its own column. Implementation details TBD — probably involves hooking mouse capture and rendering selection per-region instead of relying on terminal-native selection.

- **Editor-like text input box.** Current input handling is bare: linear typing, basic cursor. Add proper editor affordances — arrow-key cursor movement (incl. up/down across wrapped lines), Home/End, word jumps (Ctrl/Alt+arrows), shift-select, cut/copy/paste, undo/redo. Apply uniformly to every input box (scratch overlay, worker chat, orc chat). Probably worth factoring a single reusable input widget rather than duplicating per modal.

- **Ctrl+V image paste and file drag-drop.** Via OS clipboard (`arboard`) for Ctrl+V, and bracketed paste mode for Cmd+V text/path pastes and drag-drop. Both feed a shared attachment pipeline: detect image bytes or image-typed file paths, encode to base64, render as chips above the input. On send, attach as stream-json image content blocks. Works in every input box that talks to claude (scratch overlay, worker chat, orc chat).
