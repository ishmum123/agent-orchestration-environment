// End-to-end FLOW test: drives the orc TUI through every step a real user
// performs (FLOW.md) using the scripted fake_claude shim and a local bare
// git remote. Both a greenfield and a brownfield project are exercised.
//
// Per project, the test exercises:
//   - launch (FLOW 1) + state task (FLOW 2)
//   - clarify with orc via text round-trip (FLOW 4, orchestrator side)
//   - clarify with worker via real ask_user MCP modal (FLOW 4, worker side)
//   - explore via simulate_tool tool_use blocks: Read + Bash git status (FLOW 3)
//   - failure-then-fix verify cycle: write broken code, rustc fails, write
//     correct code, rustc passes (FLOW 6 + the iteration aspect of FLOW 5)
//   - queued follow-up while worker is mid-sleep — delivered as the next
//     turn after the in-flight one finishes; this is NOT a true Ctrl-C /
//     Esc interrupt (that capability lives in fake_claude but isn't
//     exercised through tmux yet)
//   - review w/ navigation j/k/J/K/]/[, whole-file `o`, editor `e`,
//     comment `c`, close `q` (FLOW 7) — captures before/after nav and
//     asserts the on-screen state actually changes
//   - rework loop after review (FLOW 7→8 transition)
//   - approve via `a` + `s` -> Done
//   - worker pushes branch to bare remote itself; assertion runs on the
//     bare remote BEFORE any harness fallback push (FLOW 8 sans gh pr create)
//   - follow-up to orc after work (local FLOW 9; PR-comment iteration excluded)
//   - mark_done MCP path: orchestrator spawns a "markme" worker that just
//     idles (no submit_for_review); orchestrator calls mark_done -> Done
//   - kill the primary (Done) worker via `x` confirm-kill -> `y`
//   - scroll keys (j/k/G/gg/PgDn/PgUp) on the orc tab
//   - control-mode toggle (`c`) on a worker tab
//   - clean quit (q + y) and `orc doctor` assertion
//
// What is intentionally NOT covered:
//   - `gh pr create` / GitHub PR review iteration (out of scope per user)
//   - restart `R` flow (capability ready via the `exit_code` action; not
//     exercised through tmux yet)
//   - true Ctrl-C / Esc interrupt of a running turn (capability ready via
//     SIGINT-aware sleep_ms; tmux-driven timing left as a follow-up)
//
// The test is heavy and serial — both projects run in one #[test] body to
// avoid tmux/state contention.

#![allow(clippy::too_many_arguments)]

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const STARTUP_WAIT_S: u64 = 12;
const POLL_TIMEOUT_S: u64 = 60;

// ---------------------------------------------------------------------------
// Fixture builders
// ---------------------------------------------------------------------------

fn run_in(dir: &Path, prog: &str, args: &[&str]) -> std::process::Output {
    let out = Command::new(prog)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("failed to run {prog} {args:?} in {dir:?}: {e}"));
    if !out.status.success() {
        panic!(
            "{prog} {args:?} in {dir:?} exited {}: stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    out
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

fn create_bare_remote(root: &Path) -> PathBuf {
    let bare = root.join("remote.git");
    std::fs::create_dir_all(&bare).unwrap();
    run_in(&bare, "git", &["init", "--bare", "-b", "main"]);
    bare
}

fn init_repo(repo: &Path, bare: &Path) {
    run_in(repo, "git", &["init", "-b", "main"]);
    run_in(repo, "git", &["config", "user.email", "flow@test.local"]);
    run_in(repo, "git", &["config", "user.name", "Flow Test"]);
    run_in(
        repo,
        "git",
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
}

fn create_greenfield_project(root: &Path, bare: &Path) -> PathBuf {
    let proj = root.join("greenfield");
    std::fs::create_dir_all(&proj).unwrap();
    init_repo(&proj, bare);
    write(&proj.join("README.md"), "# greenfield\n\nempty starter project\n");
    run_in(&proj, "git", &["add", "."]);
    run_in(&proj, "git", &["commit", "-m", "initial commit"]);
    run_in(&proj, "git", &["push", "-u", "origin", "main"]);
    proj
}

fn create_brownfield_project(root: &Path, bare: &Path) -> PathBuf {
    let proj = root.join("brownfield");
    std::fs::create_dir_all(&proj).unwrap();
    init_repo(&proj, bare);
    write(
        &proj.join("README.md"),
        "# brownfield\n\nexisting tiny project with a known duplicate-print bug.\n",
    );
    write(
        &proj.join("src/main.rs"),
        r#"fn main() {
    // BUG: the greeting is printed twice.
    println!("hello, world");
    println!("hello, world");
}
"#,
    );
    run_in(&proj, "git", &["add", "."]);
    run_in(&proj, "git", &["commit", "-m", "initial brownfield"]);
    run_in(&proj, "git", &["push", "-u", "origin", "main"]);
    proj
}

// ---------------------------------------------------------------------------
// Script JSONs
// ---------------------------------------------------------------------------

fn script_greenfield() -> Value {
    json!({
        "orchestrator": [
            // T1 — ask before acting (FLOW 4, orchestrator side).
            { "actions": [
                { "type": "text", "content":
                  "before I spawn a worker — should this be a binary crate or a library? assuming binary unless you say otherwise." }
            ]},
            // T2 — user clarified, spawn.
            { "actions": [
                { "type": "mcp_call", "tool": "spawn_session",
                  "args": { "name": "scaffolder",
                            "task": "scaffold a hello-world rust BINARY crate" } },
                { "type": "text", "content": "got it — spawned 'scaffolder' for a binary crate" }
            ]},
            // T3 — when worker calls ask_user, orc's MCP server injects a
            // synthetic user message into the orchestrator stdin asking it
            // to either race the user via answer_worker or stay quiet.
            // Stay quiet — the human will answer in the modal.
            { "actions": [
                { "type": "text", "content": "noted the worker question — letting the user handle it" }
            ]},
            // T4 — follow-up after main worker done (FLOW 9 partial).
            { "actions": [
                { "type": "text", "content": "noted, scaffolder is wrapped up" }
            ]},
            // T5 — spawn the markme worker. markme's T1 calls ask_user so
            // we exercise the answer_worker race-path.
            { "actions": [
                { "type": "mcp_call", "tool": "spawn_session",
                  "args": { "name": "markme",
                            "task": "ask the user one question, then stand by to be marked done" } },
                { "type": "text", "content": "spawned 'markme'" }
            ]},
            // T6 — auto-fired when markme's ask_user injects a synthetic
            // user message into the orchestrator. Race the user and
            // answer via answer_worker. ($WORKER_ID is markme — captured
            // from the previous spawn_session call.)
            { "actions": [
                { "type": "mcp_call", "tool": "answer_worker",
                  "args": { "session_id": "$WORKER_ID",
                            "answer": "yes, you can be marked done — orc handled this directly" } },
                { "type": "text", "content": "answered markme via answer_worker (race won)" }
            ]},
            // T7 — user follows up asking us to mark it done.
            { "actions": [
                { "type": "mcp_call", "tool": "mark_done",
                  "args": { "session_id": "$WORKER_ID",
                            "summary": "no work needed, marked done by user request" } },
                { "type": "text", "content": "marked 'markme' done" }
            ]}
        ],
        "worker_markme": [
            // markme T1: call ask_user (blocks until orc races to answer
            // via answer_worker, or until the user types in the modal).
            { "actions": [
                { "type": "mcp_call", "tool": "ask_user",
                  "args": { "session_id": "$SELF",
                            "question": "ready for me to be marked done?",
                            "context": "markme idle check" } },
                { "type": "text", "content": "got an answer, idling now" }
            ]},
            { "actions": [{ "type": "text", "content": "markme idle — turn 2" }] }
        ],
        "worker": [
            // T1 — explore + ask via REAL ask_user MCP. ask_user blocks
            // until the user answers in the modal.
            { "actions": [
                { "type": "simulate_tool", "name": "Bash",
                  "input": { "command": "ls -la" },
                  "run_cmd": "ls", "run_args": ["-la"] },
                { "type": "mcp_call", "tool": "ask_user",
                  "args": { "session_id": "$SELF",
                            "question": "should the entry point use a plain println or env_logger?",
                            "context": "scaffold question — defaulting to println if you say nothing" } },
                { "type": "text", "content": "noted, will use what you said" }
            ]},
            // T2 — main work: a beefier explore (git status, find, cat,
            // git log), then a real failure-then-fix verify cycle: write
            // broken Rust → rustc fails → write correct Rust → rustc
            // passes. Then sleep (interjection window), commit, push,
            // submit. The extra explore tools are intentional — they
            // mirror real exploration AND inflate worker log past one
            // viewport so scroll keys have somewhere to scroll.
            { "actions": [
                { "type": "simulate_tool", "name": "Bash",
                  "input": { "command": "git status" },
                  "run_cmd": "git", "run_args": ["status", "--porcelain"] },
                { "type": "simulate_tool", "name": "Bash",
                  "input": { "command": "find . -type f -not -path './.git/*'" },
                  "run_cmd": "find",
                  "run_args": [".", "-type", "f", "-not", "-path", "./.git/*"] },
                { "type": "simulate_tool", "name": "Bash",
                  "input": { "command": "cat README.md" },
                  "run_cmd": "cat", "run_args": ["README.md"] },
                { "type": "simulate_tool", "name": "Bash",
                  "input": { "command": "git log --all --oneline -10" },
                  "run_cmd": "git", "run_args": ["log", "--all", "--oneline", "-10"] },
                { "type": "write_file", "path": "Cargo.toml", "content":
                  "[package]\nname = \"greenfield\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n" },
                { "type": "write_file", "path": "src/main.rs", "content":
                  "fn main() {\n    undefined_function();\n}\n" },
                { "type": "simulate_tool", "name": "Bash",
                  "input": { "command": "rustc --emit=metadata src/main.rs (broken)" },
                  "run_cmd": "rustc",
                  "run_args": ["--emit=metadata", "src/main.rs",
                               "--out-dir", "/tmp", "--crate-name", "orc_flow_green_broken"] },
                { "type": "text", "content": "build failed — fixing the call site" },
                { "type": "write_file", "path": "src/main.rs", "content":
                  "fn main() {\n    println!(\"hello from greenfield\");\n}\n" },
                { "type": "simulate_tool", "name": "Bash",
                  "input": { "command": "rustc --emit=metadata src/main.rs" },
                  "run_cmd": "rustc",
                  "run_args": ["--emit=metadata", "src/main.rs",
                               "--out-dir", "/tmp", "--crate-name", "orc_flow_green_fixed"] },
                { "type": "sleep_ms", "ms": 4000 },
                { "type": "run_cmd", "cmd": "git", "args": ["add", "."] },
                { "type": "run_cmd", "cmd": "git",
                  "args": ["commit", "-m", "scaffold hello-world crate"] },
                { "type": "run_cmd", "cmd": "git", "args": ["push", "-u", "origin", "HEAD"] },
                { "type": "mcp_call", "tool": "submit_for_review",
                  "args": { "session_id": "$SELF", "summary": "initial scaffold ready" } },
                { "type": "text", "content": "scaffold ready for review" }
            ]},
            // T3 — interjection from the user (sent during T2's sleep, queued).
            { "actions": [
                { "type": "text", "content": "noted the interjection, will keep it in mind" }
            ]},
            // T4 — rework after review feedback.
            { "actions": [
                { "type": "write_file", "path": "src/main.rs", "content":
                  "fn main() {\n    // address review: friendlier greeting\n    println!(\"hello from greenfield — v2\");\n}\n" },
                { "type": "run_cmd", "cmd": "git", "args": ["add", "."] },
                { "type": "run_cmd", "cmd": "git",
                  "args": ["commit", "-m", "address review feedback"] },
                { "type": "run_cmd", "cmd": "git", "args": ["push", "origin", "HEAD"] },
                { "type": "mcp_call", "tool": "submit_for_review",
                  "args": { "session_id": "$SELF", "summary": "addressed review" } },
                { "type": "text", "content": "addressed review feedback, resubmitted" }
            ]}
        ]
    })
}

fn script_brownfield() -> Value {
    json!({
        "orchestrator": [
            // T1 — ask before acting.
            { "actions": [
                { "type": "text", "content":
                  "I see the duplicate println in src/main.rs. should the fix collapse to one line, or also add a test? please confirm." }
            ]},
            // T2 — clarified, spawn.
            { "actions": [
                { "type": "mcp_call", "tool": "spawn_session",
                  "args": { "name": "fixer",
                            "task": "fix duplicate println in src/main.rs (single-line fix per user, no test needed)" } },
                { "type": "text", "content": "spawned 'fixer' with the agreed scope" }
            ]},
            // T3 — absorbs the synthetic message orc injects when the
            // worker calls ask_user. Stay quiet, let the human answer.
            { "actions": [
                { "type": "text", "content": "noted the worker question — letting the user handle it" }
            ]},
            // T4 — follow-up after fixer Done.
            { "actions": [
                { "type": "text", "content": "noted, fixer is wrapped up" }
            ]},
            // T5 — spawn markme. Its T1 calls ask_user; we'll race.
            { "actions": [
                { "type": "mcp_call", "tool": "spawn_session",
                  "args": { "name": "markme",
                            "task": "ask the user one question, then stand by to be marked done" } },
                { "type": "text", "content": "spawned 'markme'" }
            ]},
            // T6 — auto-fired by markme's ask_user injection. Race-answer.
            { "actions": [
                { "type": "mcp_call", "tool": "answer_worker",
                  "args": { "session_id": "$WORKER_ID",
                            "answer": "yes, you can be marked done — orc handled this directly" } },
                { "type": "text", "content": "answered markme via answer_worker (race won)" }
            ]},
            // T7 — user asks orchestrator to mark markme done.
            { "actions": [
                { "type": "mcp_call", "tool": "mark_done",
                  "args": { "session_id": "$WORKER_ID",
                            "summary": "no work needed, marked done by user request" } },
                { "type": "text", "content": "marked 'markme' done" }
            ]}
        ],
        "worker_markme": [
            { "actions": [
                { "type": "mcp_call", "tool": "ask_user",
                  "args": { "session_id": "$SELF",
                            "question": "ready for me to be marked done?",
                            "context": "markme idle check" } },
                { "type": "text", "content": "got an answer, idling now" }
            ]},
            { "actions": [{ "type": "text", "content": "markme idle — turn 2" }] }
        ],
        "worker": [
            // T1 — explore (Read main.rs) + ask via real ask_user MCP.
            { "actions": [
                { "type": "simulate_tool", "name": "Read",
                  "input": { "file_path": "src/main.rs" },
                  "run_cmd": "cat", "run_args": ["src/main.rs"] },
                { "type": "mcp_call", "tool": "ask_user",
                  "args": { "session_id": "$SELF",
                            "question": "should I drop the BUG comment in main.rs or keep it as historical context?",
                            "context": "fix scope question" } },
                { "type": "text", "content": "noted, proceeding with that choice" }
            ]},
            // T2 — beefier explore (status, log, find, grep, diff), then
            // failure-then-fix verify cycle, then sleep (interjection
            // window), commit, push, submit.
            { "actions": [
                { "type": "simulate_tool", "name": "Bash",
                  "input": { "command": "git status" },
                  "run_cmd": "git", "run_args": ["status", "--porcelain"] },
                { "type": "simulate_tool", "name": "Bash",
                  "input": { "command": "git log --all --oneline -10" },
                  "run_cmd": "git", "run_args": ["log", "--all", "--oneline", "-10"] },
                { "type": "simulate_tool", "name": "Bash",
                  "input": { "command": "find . -type f -not -path './.git/*'" },
                  "run_cmd": "find",
                  "run_args": [".", "-type", "f", "-not", "-path", "./.git/*"] },
                { "type": "simulate_tool", "name": "Grep",
                  "input": { "pattern": "println", "path": "src/" },
                  "run_cmd": "grep",
                  "run_args": ["-r", "-n", "println", "src/"] },
                { "type": "simulate_tool", "name": "Bash",
                  "input": { "command": "git diff HEAD" },
                  "run_cmd": "git", "run_args": ["diff", "HEAD"] },
                { "type": "write_file", "path": "src/main.rs", "content":
                  "fn main() {\n    undefined_function();\n}\n" },
                { "type": "simulate_tool", "name": "Bash",
                  "input": { "command": "rustc --emit=metadata src/main.rs (broken)" },
                  "run_cmd": "rustc",
                  "run_args": ["--emit=metadata", "src/main.rs",
                               "--out-dir", "/tmp", "--crate-name", "orc_flow_brown_broken"] },
                { "type": "text", "content": "build failed — let me fix the call site" },
                { "type": "write_file", "path": "src/main.rs", "content":
                  "fn main() {\n    // fix: print only once\n    println!(\"hello, world\");\n}\n" },
                { "type": "simulate_tool", "name": "Bash",
                  "input": { "command": "rustc --emit=metadata src/main.rs" },
                  "run_cmd": "rustc",
                  "run_args": ["--emit=metadata", "src/main.rs",
                               "--out-dir", "/tmp", "--crate-name", "orc_flow_brown_fixed"] },
                { "type": "sleep_ms", "ms": 4000 },
                { "type": "run_cmd", "cmd": "git", "args": ["add", "."] },
                { "type": "run_cmd", "cmd": "git",
                  "args": ["commit", "-m", "fix: remove duplicate println"] },
                { "type": "run_cmd", "cmd": "git", "args": ["push", "-u", "origin", "HEAD"] },
                { "type": "mcp_call", "tool": "submit_for_review",
                  "args": { "session_id": "$SELF", "summary": "fix ready for review" } },
                { "type": "text", "content": "fix ready for review" }
            ]},
            // T3 — interjection (sent during T2's sleep).
            { "actions": [
                { "type": "text", "content": "noted the interjection, will keep it in mind" }
            ]},
            // T4 — rework after review feedback.
            { "actions": [
                { "type": "write_file", "path": "src/main.rs", "content":
                  "fn main() {\n    // fix: print only once + per-review note\n    println!(\"hello, world (deduped)\");\n}\n" },
                { "type": "run_cmd", "cmd": "git", "args": ["add", "."] },
                { "type": "run_cmd", "cmd": "git",
                  "args": ["commit", "-m", "address review feedback"] },
                { "type": "run_cmd", "cmd": "git", "args": ["push", "origin", "HEAD"] },
                { "type": "mcp_call", "tool": "submit_for_review",
                  "args": { "session_id": "$SELF", "summary": "addressed review" } },
                { "type": "text", "content": "addressed review feedback, resubmitted" }
            ]}
        ]
    })
}

// ---------------------------------------------------------------------------
// tmux harness
// ---------------------------------------------------------------------------

struct TmuxSession {
    label: String,
    name: String,
    home: PathBuf,
}

impl TmuxSession {
    fn launch(
        label: &str,
        name: &str,
        project_dir: &Path,
        fake_claude: &Path,
        script_path: &Path,
        stash_path: &Path,
        home: &Path,
        stderr_log: &Path,
        exit_log: &Path,
    ) -> Self {
        // Run the prebuilt orc binary directly. Going through `cargo run`
        // breaks under our HOME override because cargo is a rustup proxy
        // that wants `$HOME/.rustup/`.
        let orc_bin = PathBuf::from(env!("CARGO_BIN_EXE_orc"));
        // EDITOR=true so the review-mode `e` keybind opens a no-op editor.
        let inner = format!(
            "TMUX= ORC_CLAUDE_BIN={fake} ORC_FAKE_SCRIPT={script} ORC_FAKE_STASH={stash} HOME={home} EDITOR=/usr/bin/true {orc} -p {proj} 2>{stderr}; echo EXIT=$? > {exit}; sleep 30",
            fake = fake_claude.display(),
            script = script_path.display(),
            stash = stash_path.display(),
            home = home.display(),
            orc = orc_bin.display(),
            proj = project_dir.display(),
            stderr = stderr_log.display(),
            exit = exit_log.display(),
        );

        let out = Command::new("tmux")
            .args([
                "-L",
                label,
                "new-session",
                "-d",
                "-s",
                name,
                "-x",
                "180",
                "-y",
                "60",
                &inner,
            ])
            .env("TMUX", "")
            .output()
            .expect("tmux launch");
        if !out.status.success() {
            panic!(
                "tmux launch failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        TmuxSession {
            label: label.to_string(),
            name: name.to_string(),
            home: home.to_path_buf(),
        }
    }

    fn send_literal(&self, s: &str) {
        Command::new("tmux")
            .args(["-L", &self.label, "send-keys", "-t", &self.name, "-l", s])
            .env("TMUX", "")
            .output()
            .expect("send literal");
    }

    fn send_key(&self, key: &str) {
        Command::new("tmux")
            .args(["-L", &self.label, "send-keys", "-t", &self.name, key])
            .env("TMUX", "")
            .output()
            .expect("send key");
    }

    fn capture(&self) -> String {
        let out = Command::new("tmux")
            .args(["-L", &self.label, "capture-pane", "-t", &self.name, "-p"])
            .env("TMUX", "")
            .output()
            .expect("capture");
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Capture including scrollback history (up to 3000 lines back). orc
    /// auto-scrolls newest events to the bottom, which can push earlier
    /// content off-screen between polls; this lets us assert on it anyway.
    fn capture_with_history(&self) -> String {
        let out = Command::new("tmux")
            .args([
                "-L",
                &self.label,
                "capture-pane",
                "-t",
                &self.name,
                "-p",
                "-S",
                "-3000",
            ])
            .env("TMUX", "")
            .output()
            .expect("capture");
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn alive(&self) -> bool {
        Command::new("tmux")
            .args(["-L", &self.label, "has-session", "-t", &self.name])
            .env("TMUX", "")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn kill(&self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.label, "kill-server"])
            .env("TMUX", "")
            .output();
    }

    fn db_path(&self) -> PathBuf {
        self.home.join(".config/orc/state.db")
    }

    fn query_sessions(&self) -> Vec<(String, String, String)> {
        // (name, state_short, worktree_path)
        let out = Command::new("sqlite3")
            .args([
                self.db_path().to_str().unwrap(),
                "select name, state, worktree_path from sessions order by created_at",
            ])
            .output();
        let Ok(out) = out else { return vec![] };
        if !out.status.success() {
            return vec![];
        }
        let s = String::from_utf8_lossy(&out.stdout).to_string();
        s.lines()
            .filter_map(|l| {
                let parts: Vec<&str> = l.splitn(3, '|').collect();
                if parts.len() == 3 {
                    Some((parts[0].into(), parts[1].into(), parts[2].into()))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Wait for a session whose state JSON contains the given marker.
    /// Returns the (name, worktree) tuple of the first match.
    fn wait_for_state(&self, marker: &str, timeout: Duration) -> Option<(String, String)> {
        let start = Instant::now();
        while start.elapsed() < timeout {
            for (name, state, worktree) in self.query_sessions() {
                if state.contains(marker) {
                    return Some((name, worktree));
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Per-project flow
// ---------------------------------------------------------------------------

fn run_flow_for_project(
    label: &str,
    project_dir: &Path,
    bare_remote: &Path,
    script_json: &Value,
    fake_claude: &Path,
    workdir: &Path,
    user_task: &str,
) {
    let script_path = workdir.join(format!("{label}_script.json"));
    write(
        &script_path,
        &serde_json::to_string_pretty(script_json).unwrap(),
    );
    let stash_path = workdir.join(format!("{label}_stash.json"));
    let home = workdir.join(format!("{label}_home"));
    std::fs::create_dir_all(&home).unwrap();
    let stderr_log = workdir.join(format!("{label}_stderr.log"));
    let exit_log = workdir.join(format!("{label}_exit.log"));

    let sess_label = format!("flow-{label}");
    let tmux = TmuxSession::launch(
        &sess_label,
        "main",
        project_dir,
        fake_claude,
        &script_path,
        &stash_path,
        &home,
        &stderr_log,
        &exit_log,
    );

    // ===== FLOW 1 — Launch =====
    std::thread::sleep(Duration::from_secs(STARTUP_WAIT_S));
    let cap = tmux.capture();
    eprintln!("=== [{label}] startup capture ===\n{cap}");
    assert!(
        cap.contains("orc") && cap.contains("speak to orc"),
        "[{label}] startup did not render. capture:\n{cap}"
    );

    // ===== FLOW 2 — State the task =====
    tmux.send_literal(user_task);
    std::thread::sleep(Duration::from_millis(300));
    tmux.send_key("Enter");

    // ===== FLOW 4a — Clarify with orchestrator =====
    std::thread::sleep(Duration::from_secs(2));
    let orc_q = tmux.capture();
    eprintln!("=== [{label}] orc clarifying question ===\n{orc_q}");
    assert!(
        orc_q.contains("before I spawn") || orc_q.contains("confirm") || orc_q.contains("should"),
        "[{label}] orchestrator did not ask a clarifying question. capture:\n{orc_q}"
    );
    tmux.send_literal("t");
    std::thread::sleep(Duration::from_millis(300));
    tmux.send_literal("yes, go ahead with that scope");
    std::thread::sleep(Duration::from_millis(200));
    tmux.send_key("Enter");

    // Wait for orchestrator T2 to fire spawn_session.
    let (worker_name, _) = tmux
        .wait_for_state("Running", Duration::from_secs(POLL_TIMEOUT_S))
        .or_else(|| tmux.wait_for_state("AwaitingReview", Duration::from_secs(5)))
        .unwrap_or_else(|| panic!("[{label}] worker never spawned after orc clarification"));
    eprintln!("[{label}] worker {worker_name} spawned");

    // ===== FLOW 4b — Clarify with worker via real ask_user MCP =====
    // Worker T1 calls ask_user; orc opens a *global* AskUser modal which
    // overlays whatever tab is focused. Don't bother focusing the worker
    // first — UI session list may lag the DB and a `2` keypress can be
    // dropped if the session isn't in app.sessions yet.
    let mut saw_modal = false;
    let start = Instant::now();
    let mut last_cap = String::new();
    while start.elapsed() < Duration::from_secs(30) {
        let c = tmux.capture();
        if c.contains("orc needs your input") {
            saw_modal = true;
            break;
        }
        last_cap = c;
        std::thread::sleep(Duration::from_millis(400));
    }
    assert!(
        saw_modal,
        "[{label}] AskUser modal never appeared. last capture:\n{last_cap}"
    );
    let modal_cap = tmux.capture();
    eprintln!("=== [{label}] ask_user modal ===\n{modal_cap}");
    tmux.send_literal("default is fine, proceed");
    std::thread::sleep(Duration::from_millis(200));
    tmux.send_key("Enter");
    std::thread::sleep(Duration::from_millis(1500));

    // NOW focus the worker tab. If the broadcast hasn't yet added the
    // session to app.sessions, the `2` is a no-op and any subsequent `t`
    // would target orc instead of the worker. Retry until the action bar
    // shows "c control" — that hint only appears on worker tabs (orc tab
    // shows "^C interrupt" instead).
    let mut focused_worker = false;
    for _ in 0..15 {
        tmux.send_literal("2");
        std::thread::sleep(Duration::from_millis(400));
        if tmux.capture().contains("c control") {
            focused_worker = true;
            break;
        }
    }
    assert!(
        focused_worker,
        "[{label}] could not focus worker tab — `2` keypress kept missing.\n{}",
        tmux.capture()
    );

    // Drive worker T2 (main work) with a `t` message on the worker tab.
    tmux.send_literal("t");
    std::thread::sleep(Duration::from_millis(300));
    tmux.send_literal("ok proceed with the work");
    std::thread::sleep(Duration::from_millis(200));
    tmux.send_key("Enter");

    // ===== FLOW 5 — Mid-stream interjection during T2's sleep =====
    // T2 has sleep_ms=4000 near the start. Wait ~1.5s, then send another
    // message. fake_claude reads it after T2 completes, runs T3.
    std::thread::sleep(Duration::from_millis(1500));
    tmux.send_literal("t");
    std::thread::sleep(Duration::from_millis(300));
    tmux.send_literal("by the way, also keep the tone friendly");
    std::thread::sleep(Duration::from_millis(200));
    tmux.send_key("Enter");

    // ===== FLOW 3 + 6 — Wait for explore + verify + push + AwaitingReview =====
    // While polling for AwaitingReview, also watch for the rustc failure
    // output so we can confirm the failure-then-fix cycle was visible to orc.
    // Poll fast (75ms) — once worker auto-scrolls past the error block, it
    // can leave the visible window quickly.
    let mut saw_error = false;
    let mut worktree = String::new();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(POLL_TIMEOUT_S) {
        let cap = tmux.capture();
        if !saw_error
            && (cap.contains("error[E0425]")
                || cap.contains("cannot find function")
                || cap.contains("undefined_function"))
        {
            saw_error = true;
        }
        for (name, state, wt) in tmux.query_sessions() {
            if name == worker_name && state.contains("AwaitingReview") {
                worktree = wt;
                break;
            }
        }
        if !worktree.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(75));
    }
    if worktree.is_empty() {
        let dump = tmux.capture();
        let sessions = tmux.query_sessions();
        panic!(
            "[{label}] worker never reached AwaitingReview. sessions={sessions:?}\ncapture:\n{dump}"
        );
    }
    // FLOW 6 (verify) — the worker T2 script writes broken Rust, runs
    // rustc (real subprocess, exits non-zero), writes fixed Rust, runs
    // rustc again (succeeds), then commits the fixed version. Reaching
    // AwaitingReview implies all of T2 ran. We log whether the live
    // capture caught the error string (it can scroll past in fast runs).
    if saw_error {
        eprintln!("[{label}] failure-fix: rustc error visible in capture during T2");
    } else {
        eprintln!(
            "[{label}] failure-fix: rustc error scrolled past before poll caught it (T2 still ran fully — worker reached AwaitingReview)"
        );
    }
    eprintln!("[{label}] worker awaiting review at {worktree}");

    // FLOW 8 — verify the worker pushed its branch BEFORE the harness ever
    // touches the remote. After T2, the bare remote should already have
    // `orc/<worker_name>` even though we haven't done any harness-side push.
    let pre_branches = run_in(bare_remote, "git", &["branch", "--list"]).stdout;
    let pre_branches_str = String::from_utf8_lossy(&pre_branches);
    eprintln!("[{label}] bare remote branches after worker push:\n{pre_branches_str}");
    assert!(
        pre_branches_str.lines().any(|l| l.contains(&format!("orc/{worker_name}"))),
        "[{label}] worker did not push its branch — bare remote has only:\n{pre_branches_str}"
    );

    // ===== FLOW 7 — Review with full keymap coverage + nav assertions =====
    tmux.send_literal("r");
    std::thread::sleep(Duration::from_millis(800));
    let review_cap_initial = tmux.capture();
    eprintln!("=== [{label}] review modal initial ===\n{review_cap_initial}");
    assert!(
        review_cap_initial.contains("review")
            || review_cap_initial.contains("diff")
            || review_cap_initial.contains("+"),
        "[{label}] review modal did not render. capture:\n{review_cap_initial}"
    );

    // Line nav (j×3): expect cursor moved → capture differs.
    for _ in 0..3 {
        tmux.send_literal("j");
        std::thread::sleep(Duration::from_millis(80));
    }
    let after_j = tmux.capture();
    assert!(
        after_j != review_cap_initial,
        "[{label}] j×3 produced no visible change in review modal"
    );
    tmux.send_literal("k");
    std::thread::sleep(Duration::from_millis(100));

    // Hunk + file nav. For multi-file projects, every key is asserted;
    // for single-file/single-hunk projects (brownfield), the keys are
    // exercised but the assertions are skipped because nav has nowhere to
    // go.
    let multi_file = review_cap_initial.contains("Cargo.toml")
        && review_cap_initial.contains("main.rs");

    let before_J = tmux.capture();
    tmux.send_literal("J");
    std::thread::sleep(Duration::from_millis(150));
    let after_J = tmux.capture();
    if multi_file {
        assert!(after_J != before_J, "[{label}] J produced no visible change");
    }
    let before_K = tmux.capture();
    tmux.send_literal("K");
    std::thread::sleep(Duration::from_millis(150));
    let after_K = tmux.capture();
    if multi_file {
        assert!(after_K != before_K, "[{label}] K produced no visible change");
    }

    let before_bracket = tmux.capture();
    tmux.send_literal("]");
    std::thread::sleep(Duration::from_millis(150));
    let after_bracket = tmux.capture();
    if multi_file {
        assert!(
            after_bracket != before_bracket,
            "[{label}] ] (next file) produced no visible change"
        );
    }
    let before_back_bracket = tmux.capture();
    tmux.send_literal("[");
    std::thread::sleep(Duration::from_millis(150));
    let after_back_bracket = tmux.capture();
    if multi_file {
        assert!(
            after_back_bracket != before_back_bracket,
            "[{label}] [ (prev file) produced no visible change"
        );
    }

    // Whole-file view toggle.
    tmux.send_literal("o");
    std::thread::sleep(Duration::from_millis(500));
    let whole_cap = tmux.capture();
    eprintln!("=== [{label}] whole-file view ===\n{whole_cap}");
    assert!(
        whole_cap != review_cap_initial,
        "[{label}] o (whole-file view) produced no visible change"
    );
    // Back to diff view.
    tmux.send_literal("o");
    std::thread::sleep(Duration::from_millis(400));
    let back_to_diff = tmux.capture();
    assert!(
        back_to_diff != whole_cap,
        "[{label}] o (back to diff) produced no visible change"
    );

    // Editor (`e`) — EDITOR=/usr/bin/true exits immediately, ratatui
    // resumes. Just exercise the path; no on-screen assertion (the
    // suspend-and-resume cycle re-renders the same view).
    tmux.send_literal("e");
    std::thread::sleep(Duration::from_millis(900));

    // Hunk approval toggle off-then-on path: footer shows "N approvals".
    // Press `a` once → 1 approval; press again (rejection / toggle off) →
    // 0 approvals. Then leave it un-approved when we close (this round
    // doesn't approve; we'll approve in the second-review pass).
    fn extract_approvals(cap: &str) -> Option<u32> {
        cap.lines()
            .find_map(|l| {
                let l = l.trim();
                let idx = l.find(" approvals")?;
                let count_str = l[..idx].rsplit_once(' ').map(|(_, n)| n).unwrap_or("");
                count_str.trim().parse::<u32>().ok()
            })
    }
    tmux.send_literal("a");
    std::thread::sleep(Duration::from_millis(200));
    let approvals_after_a = extract_approvals(&tmux.capture()).unwrap_or(0);
    assert!(
        approvals_after_a >= 1,
        "[{label}] expected ≥1 approval after `a`; got {approvals_after_a}"
    );
    tmux.send_literal("a");
    std::thread::sleep(Duration::from_millis(200));
    let approvals_after_toggle_off = extract_approvals(&tmux.capture()).unwrap_or(99);
    assert_eq!(
        approvals_after_toggle_off, 0,
        "[{label}] expected 0 approvals after toggle-off; got {approvals_after_toggle_off}"
    );

    // Comment list grows: c → text → Enter, repeat. Footer "N comments"
    // should increment.
    fn extract_comments(cap: &str) -> Option<u32> {
        cap.lines()
            .find_map(|l| {
                let l = l.trim();
                let idx = l.find(" comments")?;
                let count_str = l[..idx].rsplit_once(' ').map(|(_, n)| n).unwrap_or("");
                count_str.trim().parse::<u32>().ok()
            })
    }

    tmux.send_literal("c");
    std::thread::sleep(Duration::from_millis(400));
    tmux.send_literal("please make this friendlier");
    std::thread::sleep(Duration::from_millis(200));
    tmux.send_key("Enter");
    std::thread::sleep(Duration::from_millis(300));
    let after_first_comment = extract_comments(&tmux.capture()).unwrap_or(0);
    assert!(
        after_first_comment >= 1,
        "[{label}] expected ≥1 comment after first c; got {after_first_comment}"
    );

    // Move down a few lines so the second comment lands somewhere new.
    for _ in 0..2 {
        tmux.send_literal("j");
        std::thread::sleep(Duration::from_millis(60));
    }
    tmux.send_literal("c");
    std::thread::sleep(Duration::from_millis(400));
    tmux.send_literal("and consider a comment here too");
    std::thread::sleep(Duration::from_millis(200));
    tmux.send_key("Enter");
    std::thread::sleep(Duration::from_millis(300));
    let after_second_comment = extract_comments(&tmux.capture()).unwrap_or(0);
    assert!(
        after_second_comment >= after_first_comment + 1,
        "[{label}] expected comment count to grow past {after_first_comment}; got {after_second_comment}"
    );

    // Close review with `q`.
    tmux.send_literal("q");
    std::thread::sleep(Duration::from_millis(400));
    let after_close = tmux.capture();
    assert!(
        !after_close.contains("j/k line"),
        "[{label}] review modal did not close with q. capture:\n{after_close}"
    );

    // ===== FLOW 8 transition — Send rework, expect resubmit =====
    tmux.send_literal("t");
    std::thread::sleep(Duration::from_millis(300));
    tmux.send_literal("please address the comment and resubmit");
    std::thread::sleep(Duration::from_millis(200));
    tmux.send_key("Enter");

    // Wait for the second AwaitingReview (after T4 reworks + pushes again).
    std::thread::sleep(Duration::from_secs(3));
    let mut second_review_seen = false;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(POLL_TIMEOUT_S) {
        let sessions = tmux.query_sessions();
        if let Some((_, state, _)) = sessions.iter().find(|(n, _, _)| n == &worker_name) {
            if state.contains("AwaitingReview") {
                second_review_seen = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        second_review_seen,
        "[{label}] worker did not reach second AwaitingReview"
    );

    // Re-open review, approve hunk(s), submit -> auto-Done.
    tmux.send_literal("r");
    std::thread::sleep(Duration::from_millis(700));
    tmux.send_literal("a");
    std::thread::sleep(Duration::from_millis(150));
    tmux.send_literal("s");
    std::thread::sleep(Duration::from_millis(400));

    tmux.wait_for_state("Done", Duration::from_secs(POLL_TIMEOUT_S))
        .unwrap_or_else(|| panic!("[{label}] worker never reached Done"));

    // ===== Control mode toggle on the worker tab =====
    // [WATCH]/[CTRL] glyphs only appear in render_tabs which the prod
    // layout doesn't actually render today (header shows just the focused
    // tab name). So assert the toggle by reading the `mode` column of the
    // sessions table directly.
    let read_mode = |name: &str| -> Option<String> {
        let out = Command::new("sqlite3")
            .args([
                tmux.db_path().to_str().unwrap(),
                &format!("select mode from sessions where name='{name}'"),
            ])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    };
    assert_eq!(
        read_mode(&worker_name).as_deref(),
        Some("Watch"),
        "[{label}] expected mode=Watch pre-toggle"
    );
    tmux.send_literal("c");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        read_mode(&worker_name).as_deref(),
        Some("Control"),
        "[{label}] expected mode=Control after first c"
    );
    tmux.send_literal("c");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        read_mode(&worker_name).as_deref(),
        Some("Watch"),
        "[{label}] expected mode=Watch after second c"
    );

    // ===== FLOW 9 (local follow-up — not PR-comment iteration) =====
    tmux.send_literal("1");
    std::thread::sleep(Duration::from_millis(200));
    tmux.send_literal("t");
    std::thread::sleep(Duration::from_millis(300));
    tmux.send_literal(&format!("thanks — worker {worker_name} looks good"));
    std::thread::sleep(Duration::from_millis(200));
    tmux.send_key("Enter");
    std::thread::sleep(Duration::from_millis(800));

    // ===== Scroll keys =====
    // The events panel header reads "[N–M / T lines · E entries]". If
    // total lines T exceed the visible window, scroll keys move N–M.
    // Use the worker tab (lots of tool output) and only assert when there's
    // actually content to scroll past.
    tmux.send_literal("2");
    std::thread::sleep(Duration::from_millis(400));
    fn parse_window(cap: &str) -> Option<(u32, u32, u32)> {
        // Match `[N–M / T lines` (note: en-dash).
        for l in cap.lines() {
            if !l.contains("events") || !l.contains("lines") {
                continue;
            }
            let bracket_idx = l.find('[')?;
            let body = &l[bracket_idx + 1..];
            let slash_idx = body.find(" / ")?;
            let range = &body[..slash_idx];
            let after_slash = &body[slash_idx + 3..];
            let lines_idx = after_slash.find(" lines")?;
            let total: u32 = after_slash[..lines_idx].trim().parse().ok()?;
            let dash_idx = range.find('–').or_else(|| range.find('-'))?;
            let start: u32 = range[..dash_idx].trim().parse().ok()?;
            // Skip the dash bytes (en-dash is 3 bytes in UTF-8).
            let after_dash = &range[dash_idx + range[dash_idx..].chars().next()?.len_utf8()..];
            let end: u32 = after_dash.trim().parse().ok()?;
            return Some((start, end, total));
        }
        None
    }
    let (s0, e0, t0) = parse_window(&tmux.capture()).unwrap_or((0, 0, 0));
    eprintln!("[{label}] scroll initial window: {s0}-{e0}/{t0}");

    // Only assert if there's something to scroll.
    let visible_rows = e0.saturating_sub(s0).saturating_add(1);
    if t0 > visible_rows {
        tmux.send_key("PageUp");
        std::thread::sleep(Duration::from_millis(200));
        tmux.send_key("PageUp");
        std::thread::sleep(Duration::from_millis(200));
        let (s1, e1, _) = parse_window(&tmux.capture()).unwrap_or((s0, e0, t0));
        assert!(
            (s1, e1) != (s0, e0),
            "[{label}] PageUp didn't move scroll window. before:{s0}-{e0} after:{s1}-{e1}"
        );
        tmux.send_literal("g");
        std::thread::sleep(Duration::from_millis(80));
        tmux.send_literal("g");
        std::thread::sleep(Duration::from_millis(200));
        let (s2, _, _) = parse_window(&tmux.capture()).unwrap_or((s1, e1, t0));
        assert_eq!(s2, 1, "[{label}] gg chord should jump to top (start=1); got start={s2}");
        tmux.send_literal("G");
        std::thread::sleep(Duration::from_millis(200));
        let (_, e3, _) = parse_window(&tmux.capture()).unwrap_or((1, e0, t0));
        assert_eq!(e3, t0, "[{label}] G should jump to bottom (end={t0}); got end={e3}");
    } else {
        eprintln!(
            "[{label}] worker tab content fits ({t0} lines, {visible_rows} visible) — \
             scroll keys exercised but no scroll possible to assert"
        );
        // Still exercise the keys; they're no-ops when content fits.
        tmux.send_key("PageUp");
        std::thread::sleep(Duration::from_millis(80));
        tmux.send_literal("g");
        std::thread::sleep(Duration::from_millis(60));
        tmux.send_literal("g");
        std::thread::sleep(Duration::from_millis(80));
        tmux.send_literal("G");
        std::thread::sleep(Duration::from_millis(80));
    }

    // Return focus to orc tab for the follow-up message.
    tmux.send_literal("1");
    std::thread::sleep(Duration::from_millis(300));

    // ===== answer_worker race + mark_done MCP path =====
    // Spawn the markme worker (orchestrator T5). markme's T1 calls
    // ask_user, which (a) opens an AskUser modal AND (b) injects a
    // synthetic message into the orchestrator. Orchestrator T6 races and
    // answers via answer_worker; the modal closes WITHOUT user input.
    tmux.send_literal("t");
    std::thread::sleep(Duration::from_millis(300));
    tmux.send_literal("spawn an idle markme worker for me");
    std::thread::sleep(Duration::from_millis(200));
    tmux.send_key("Enter");

    // Watch for the orchestrator's T6 output — that's the line orc tab
    // shows after answer_worker resolves. The "answered by orc" log lives
    // in the markme tab; we don't switch focus there because the
    // orchestrator-side signal is sufficient and easier to detect.
    let mut race_won = false;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        let cap = tmux.capture();
        if cap.contains("answered markme via answer_worker") || cap.contains("race won") {
            race_won = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    assert!(
        race_won,
        "[{label}] orc never race-answered markme via answer_worker.\nlast capture:\n{}",
        tmux.capture()
    );

    // Multi-worker concurrency: markme should be Running while the primary
    // is Done. Both should be present in the DB at this moment.
    let multi = tmux.query_sessions();
    let primary_done = multi
        .iter()
        .any(|(n, s, _)| n == &worker_name && s.contains("Done"));
    let markme_running = multi
        .iter()
        .any(|(n, s, _)| n == "markme" && (s.contains("Running") || s.contains("Done")));
    assert!(
        primary_done && markme_running,
        "[{label}] expected primary Done + markme alive concurrently; got {multi:?}"
    );

    // Send the follow-up so orchestrator T7 calls mark_done.
    tmux.send_literal("t");
    std::thread::sleep(Duration::from_millis(300));
    tmux.send_literal("ok now please mark markme done");
    std::thread::sleep(Duration::from_millis(200));
    tmux.send_key("Enter");

    let mut markme_done = false;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        if tmux
            .query_sessions()
            .iter()
            .any(|(n, s, _)| n == "markme" && s.contains("Done"))
        {
            markme_done = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    assert!(
        markme_done,
        "[{label}] markme never reached Done via mark_done MCP"
    );

    // ===== Kill flow — `x` confirm-kill on the primary (Done) worker =====
    // Tab to the primary worker (still alive in Done state until idle
    // cleanup). `x` opens ConfirmKill modal; `y` confirms removal.
    tmux.send_literal("2");
    std::thread::sleep(Duration::from_millis(300));
    tmux.send_literal("x");
    std::thread::sleep(Duration::from_millis(400));
    let kill_modal = tmux.capture();
    eprintln!("=== [{label}] kill confirm modal ===\n{kill_modal}");
    assert!(
        kill_modal.contains("confirm kill") || kill_modal.contains("kill"),
        "[{label}] confirm-kill modal not visible. capture:\n{kill_modal}"
    );
    tmux.send_literal("y");
    std::thread::sleep(Duration::from_millis(1500));
    let post_kill_sessions = tmux.query_sessions();
    assert!(
        !post_kill_sessions.iter().any(|(n, _, _)| n == &worker_name),
        "[{label}] worker {worker_name} not removed after kill: {post_kill_sessions:?}"
    );

    // ===== Final capture + quit =====
    let final_cap = tmux.capture();
    eprintln!("=== [{label}] final tui capture ===\n{final_cap}");

    tmux.send_literal("q");
    std::thread::sleep(Duration::from_millis(300));
    tmux.send_literal("y");
    std::thread::sleep(Duration::from_secs(2));

    let exit_str = std::fs::read_to_string(&exit_log).unwrap_or_default();
    assert!(
        exit_str.contains("EXIT=0"),
        "[{label}] orc did not exit cleanly: {exit_str}\nstderr:\n{}",
        std::fs::read_to_string(&stderr_log).unwrap_or_default()
    );

    // ===== Final assertions =====
    let bare_branches = run_in(bare_remote, "git", &["branch", "--list"]).stdout;
    let bare_branches_str = String::from_utf8_lossy(&bare_branches);
    eprintln!("[{label}] final bare remote branches:\n{bare_branches_str}");
    assert!(
        bare_branches_str
            .lines()
            .any(|l| l.contains(&format!("orc/{worker_name}"))),
        "[{label}] worker branch missing from bare remote:\n{bare_branches_str}"
    );

    // ===== orc doctor on the test HOME =====
    let orc_bin = PathBuf::from(env!("CARGO_BIN_EXE_orc"));
    let doctor = Command::new(orc_bin)
        .arg("doctor")
        .env("HOME", &tmux.home)
        .output()
        .expect("doctor run");
    let doctor_out = format!(
        "{}{}",
        String::from_utf8_lossy(&doctor.stdout),
        String::from_utf8_lossy(&doctor.stderr)
    );
    eprintln!("=== [{label}] doctor ===\n{doctor_out}");
    assert!(
        doctor_out.contains("all checks passed"),
        "[{label}] doctor not green:\n{doctor_out}"
    );

    tmux.kill();
    assert!(!tmux.alive(), "[{label}] tmux session lingered");
}

// ---------------------------------------------------------------------------
// Test entry
// ---------------------------------------------------------------------------

#[test]
fn flow_e2e_full_journey() {
    let fake_bin = PathBuf::from(env!("CARGO_BIN_EXE_fake_claude"));
    let workdir = tempfile::tempdir().expect("workdir");
    let root = workdir.path().to_path_buf();
    // Leak workdir UP FRONT so panics during the run leave the tree intact
    // for forensic inspection.
    eprintln!("workdir: {}", root.display());
    std::mem::forget(workdir);

    // Each project gets its own bare remote so we can assert independently.
    let bare_green = create_bare_remote(&root.join("greenfield_remote"));
    let bare_brown = create_bare_remote(&root.join("brownfield_remote"));

    // Per-project parent dir so each project's `.orc-worktrees/` is
    // isolated. Without this, both projects would share
    // `<root>/projects/.orc-worktrees/` and a session named "markme" in
    // both would collide on the worktree path.
    let green = create_greenfield_project(&root.join("green_parent"), &bare_green);
    let brown = create_brownfield_project(&root.join("brown_parent"), &bare_brown);

    eprintln!("=== greenfield run ===");
    run_flow_for_project(
        "green",
        &green,
        &bare_green,
        &script_greenfield(),
        &fake_bin,
        &root,
        "scaffold a hello-world rust crate for me",
    );

    eprintln!("=== brownfield run ===");
    run_flow_for_project(
        "brown",
        &brown,
        &bare_brown,
        &script_brownfield(),
        &fake_bin,
        &root,
        "fix the bug where main.rs prints the greeting twice",
    );

    // workdir was already leaked at the top so panics could leave evidence.
}
