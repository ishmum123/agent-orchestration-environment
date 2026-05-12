// End-to-end test for the scratch overlay Ctrl+N "attach as worker"
// flow. Drives the real orc binary via tmux + fake_claude (echo mode),
// presses `?` to open the overlay, sends a message, then Ctrl+N. Asserts
// that a new worker session lands in the DB whose shape matches what a
// regular `c` chat / `spawn_session` would have produced (worktree on
// disk, branch under `orc/`, MCP wiring inferable from the session row).
//
// Echo-mode fake_claude does NOT produce a parseable {slug, task} JSON
// reply, so this test exercises the deterministic fallback path:
// slug = `scratch-<HHMMSS>`, task = the raw echo text. That's the
// hostile case — if it works here, the happy path (real claude returns
// JSON) is just a name change.

#![allow(clippy::too_many_arguments)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const STARTUP_WAIT_S: u64 = 8;
const POLL_TIMEOUT_S: u64 = 30;

fn run_in(dir: &Path, prog: &str, args: &[&str]) {
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
}

fn write_file(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

fn make_project(root: &Path) -> PathBuf {
    let proj = root.join("scratch_project");
    std::fs::create_dir_all(&proj).unwrap();
    run_in(&proj, "git", &["init", "-q", "-b", "main"]);
    run_in(&proj, "git", &["config", "user.email", "scratch@test.local"]);
    run_in(&proj, "git", &["config", "user.name", "Scratch Test"]);
    write_file(&proj.join("README.md"), "# scratch\n");
    run_in(&proj, "git", &["add", "."]);
    run_in(&proj, "git", &["commit", "-q", "-m", "init"]);
    proj
}

struct Tmux {
    label: String,
    name: String,
}

impl Tmux {
    fn launch(
        label: &str,
        name: &str,
        project_dir: &Path,
        fake_claude: &Path,
        home: &Path,
        stderr_log: &Path,
        exit_log: &Path,
    ) -> Self {
        let orc_bin = PathBuf::from(env!("CARGO_BIN_EXE_orc"));
        let inner = format!(
            "TMUX= ORC_CLAUDE_BIN={fake} HOME={home} EDITOR=/usr/bin/true {orc} -p {proj} 2>{stderr}; echo EXIT=$? > {exit}; sleep 30",
            fake = fake_claude.display(),
            home = home.display(),
            orc = orc_bin.display(),
            proj = project_dir.display(),
            stderr = stderr_log.display(),
            exit = exit_log.display(),
        );

        let _ = Command::new("tmux")
            .args(["-L", label, "kill-server"])
            .env("TMUX", "")
            .output();

        let out = Command::new("tmux")
            .args([
                "-L", label, "new-session", "-d", "-s", name,
                "-x", "160", "-y", "50", &inner,
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
        Tmux {
            label: label.to_string(),
            name: name.to_string(),
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

    fn kill(&self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.label, "kill-server"])
            .env("TMUX", "")
            .output();
    }
}

fn query_sessions(db: &Path) -> Vec<Vec<String>> {
    // name|state|worktree_path|branch|base_commit|model|task
    let out = Command::new("sqlite3")
        .args([
            db.to_str().unwrap(),
            "select name, state, worktree_path, branch, base_commit, model, task from sessions order by created_at",
        ])
        .output();
    let Ok(out) = out else { return vec![] };
    if !out.status.success() {
        return vec![];
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.splitn(7, '|').map(str::to_string).collect())
        .collect()
}

fn wait_for_session(db: &Path, timeout: Duration) -> Option<Vec<String>> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        for row in query_sessions(db) {
            if row.len() == 7 {
                return Some(row);
            }
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    None
}

#[test]
fn ctrl_n_attach_promotes_scratch_chat_to_regular_worker() {
    let workdir = tempfile::tempdir().expect("tempdir");
    let root = workdir.path();
    let project = make_project(root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let stderr_log = root.join("stderr.log");
    let exit_log = root.join("exit.log");

    let fake = PathBuf::from(env!("CARGO_BIN_EXE_fake_claude"));

    let tmux = Tmux::launch(
        "scratch-attach",
        "main",
        &project,
        &fake,
        &home,
        &stderr_log,
        &exit_log,
    );

    std::thread::sleep(Duration::from_secs(STARTUP_WAIT_S));
    let startup = tmux.capture();
    assert!(
        startup.contains("orc"),
        "startup did not render. capture:\n{startup}"
    );

    // The fresh-project NewTask modal grabs focus. Dismiss it so global
    // keys (?) are routed.
    tmux.send_key("Escape");
    std::thread::sleep(Duration::from_millis(400));

    // Open scratch overlay.
    tmux.send_literal("?");
    std::thread::sleep(Duration::from_millis(800));
    let overlay = tmux.capture();
    assert!(
        overlay.contains("scratch claude (sonnet)"),
        "overlay did not open. capture:\n{overlay}"
    );
    assert!(
        overlay.contains("Ctrl+n attach"),
        "action bar should say `Ctrl+n attach`. capture:\n{overlay}"
    );

    // Send a message into the scratch channel.
    tmux.send_literal("brainstorm rate limiter design");
    std::thread::sleep(Duration::from_millis(300));
    tmux.send_key("Enter");
    // Wait for the echo to land.
    let reply_deadline = Instant::now() + Duration::from_secs(8);
    let mut saw_echo = false;
    while Instant::now() < reply_deadline {
        if tmux.capture().contains("brainstorm rate limiter design") {
            saw_echo = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    assert!(saw_echo, "scratch echo never arrived");

    // Confirm no worker exists yet — the channel is sealed off.
    let db = project.join(".orc/state.db");
    let pre = query_sessions(&db);
    assert!(
        pre.is_empty(),
        "no worker should exist before Ctrl+N. got: {pre:?}"
    );

    // Press Ctrl+N — attach.
    tmux.send_key("C-n");

    // Wait for a worker session row to appear in the DB.
    let row = wait_for_session(&db, Duration::from_secs(POLL_TIMEOUT_S))
        .unwrap_or_else(|| panic!("worker never appeared in DB. capture:\n{}", tmux.capture()));

    let name = &row[0];
    let state = &row[1];
    let worktree = &row[2];
    let branch = &row[3];
    let base_commit = &row[4];
    let model = &row[5];
    let task = &row[6];

    // Fallback slug path: echo-mode fake_claude doesn't return JSON.
    assert!(
        name.starts_with("scratch-"),
        "slug fallback expected (echo-mode fake_claude). got name={name}"
    );

    // Worktree on disk, under .orc-worktrees/<name>.
    assert!(
        Path::new(worktree).exists(),
        "worktree dir missing: {worktree}"
    );
    assert!(
        worktree.contains(".orc-worktrees"),
        "worktree path should live under .orc-worktrees. got: {worktree}"
    );
    assert!(
        worktree.ends_with(name.as_str()),
        "worktree name should match session name. got: {worktree}"
    );

    // Branch shape: `orc/<name>`.
    assert_eq!(
        branch,
        &format!("orc/{name}"),
        "branch should follow orc/<name>. got: {branch}"
    );

    // base_commit is a non-empty git hash.
    assert!(
        base_commit.len() >= 7 && base_commit.chars().all(|c| c.is_ascii_hexdigit()),
        "base_commit should be a git hash. got: {base_commit}"
    );

    // Default model for scratch attach is sonnet.
    assert_eq!(model, "sonnet", "scratch attach uses sonnet by default");

    // Task is non-empty and reflects the echo-mode raw reply (fallback
    // path uses the entire summary text). Real claude with JSON output
    // would put the parsed `task` field here instead.
    assert!(
        !task.is_empty(),
        "task should be non-empty"
    );

    // Session is alive (Running state).
    assert!(
        state.contains("Running") || state.contains("Awaiting"),
        "fresh worker should be Running. got: {state}"
    );

    // Structural equivalence check: spawn an MCP-style worker via the
    // orchestrator path would produce the same column layout. Here we
    // assert the worker has every field a regular worker has set —
    // none are NULL or empty placeholders.
    assert!(!name.is_empty());
    assert!(!worktree.is_empty());
    assert!(!branch.is_empty());
    assert!(!model.is_empty());
    assert!(!task.is_empty());

    // Action bar after attach should look like a worker-tab bar — the
    // overlay is closed.
    let after = tmux.capture();
    assert!(
        !after.contains("scratch claude (sonnet)"),
        "overlay should be closed after Ctrl+N. capture:\n{after}"
    );
    // The focused tab should be the worker (visible task in header).
    assert!(
        after.contains(name.as_str()),
        "new worker tab should be visible. capture:\n{after}"
    );

    // Clean exit.
    tmux.send_literal("q");
    std::thread::sleep(Duration::from_millis(600));
    tmux.send_literal("y");
    std::thread::sleep(Duration::from_secs(3));
    let exit = std::fs::read_to_string(&exit_log).unwrap_or_default();
    assert!(
        exit.trim() == "EXIT=0",
        "orc did not exit cleanly. exit_log={exit:?} stderr={}",
        std::fs::read_to_string(&stderr_log).unwrap_or_default()
    );

    tmux.kill();
}
