// Tmux session lifecycle: create, attach, kill worker panes.

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;
use tokio::process::Command;
use tokio::sync::mpsc;

// ── private helper ────────────────────────────────────────────────────────────

async fn run_tmux(args: &[&str]) -> Result<String> {
    let output = Command::new("tmux").args(args).output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux {:?} failed: {}", args, stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// ── config generation ─────────────────────────────────────────────────────────

fn tmux_conf_content() -> &'static str {
    "# orc worker session config\n\
     set -g status off\n\
     set -g prefix None\n\
     unbind C-b\n\
     bind -n C-q detach-client\n\
     set -g mouse off\n\
     set -g default-terminal \"screen-256color\"\n"
}

fn conf_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("orc-tmux-{name}.conf"))
}

async fn write_conf(name: &str) -> Result<std::path::PathBuf> {
    let path = conf_path(name);
    fs::write(&path, tmux_conf_content()).await?;
    Ok(path)
}

// ── public API ────────────────────────────────────────────────────────────────

/// Create a detached tmux session with custom config.
pub async fn create_session(name: &str, working_dir: &Path) -> Result<()> {
    let conf = write_conf(name).await?;
    let conf_str = conf.to_string_lossy();
    let dir_str = working_dir.to_string_lossy();
    run_tmux(&[
        "-f",
        &conf_str,
        "new-session",
        "-d",
        "-s",
        name,
        "-c",
        &dir_str,
    ])
    .await?;
    Ok(())
}

/// Kill a tmux session.
pub async fn kill_session(name: &str) -> Result<()> {
    run_tmux(&["kill-session", "-t", name]).await?;
    // Best-effort cleanup of the conf file; ignore errors.
    let _ = fs::remove_file(conf_path(name)).await;
    Ok(())
}

/// Check if a tmux session exists.
pub async fn has_session(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Send text to a tmux session via load-buffer + paste-buffer.
pub async fn send_text(name: &str, text: &str) -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("orc-tmux-buf-{name}.txt"));
    fs::write(&tmp, text).await?;
    let tmp_str = tmp.to_string_lossy();
    run_tmux(&["load-buffer", &tmp_str]).await?;
    run_tmux(&["paste-buffer", "-t", name]).await?;
    let _ = fs::remove_file(&tmp).await;
    Ok(())
}

/// Send keys directly (for Enter, C-c, etc.)
pub async fn send_keys(name: &str, keys: &[&str]) -> Result<()> {
    let mut args = vec!["send-keys", "-t", name];
    args.extend_from_slice(keys);
    run_tmux(&args).await?;
    Ok(())
}

/// Start pipe-pane to capture terminal output to a log file.
pub async fn start_pipe_pane(name: &str, logfile: &Path) -> Result<()> {
    let cmd = format!("cat >> {}", logfile.to_string_lossy());
    run_tmux(&["pipe-pane", "-t", name, "-o", &cmd]).await?;
    Ok(())
}

/// Read last N lines from a pipe-pane log file.
pub async fn read_pipe_log(logfile: &Path, last_n: usize) -> Result<Vec<String>> {
    let content = fs::read_to_string(logfile).await?;
    let lines: Vec<String> = content
        .lines()
        .rev()
        .take(last_n)
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Ok(lines)
}

/// Poll a pipe-pane log file until any of the given marker substrings appears
/// in it, or until the timeout expires.
///
/// Returns `Ok(true)` when a marker is seen, `Ok(false)` on timeout. Errors only
/// on hard IO failures other than "file not yet created" (which is treated as
/// "not ready, keep polling").
pub async fn wait_for_log_match(
    log_path: &Path,
    markers: &[&str],
    timeout: Duration,
) -> Result<bool> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(content) = fs::read_to_string(log_path).await {
            let cleaned = strip_ansi(&content);
            if markers.iter().any(|m| cleaned.contains(m)) {
                return Ok(true);
            }
        }
        if std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Strip ANSI escape sequences and control chars from a line.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    while let Some(&pc) = chars.peek() {
                        chars.next();
                        if pc.is_ascii_alphabetic() || pc == '~' {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(&pc) = chars.peek() {
                        chars.next();
                        if pc == '\x07' {
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
        } else if c == '\r' {
            // ignore CR
        } else if (c as u32) < 0x20 && c != '\t' {
            // skip other control chars
        } else {
            out.push(c);
        }
    }
    out
}

/// Spawn a task that tails a pipe-pane log file and forwards new lines.
/// Sends `(session_id, cleaned_line)` for every newline-terminated chunk.
/// Exits when the channel is closed or after persistent file errors.
pub fn spawn_log_tailer(
    session_id: String,
    log_path: PathBuf,
    tx: mpsc::Sender<(String, String)>,
) {
    tokio::spawn(async move {
        // Wait up to ~30s for the log file to appear.
        for _ in 0..60 {
            if log_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        if !log_path.exists() {
            return;
        }

        let mut offset: u64 = 0;
        let mut leftover = String::new();
        let mut error_streak = 0;

        loop {
            if tx.is_closed() {
                return;
            }
            match tokio::fs::metadata(&log_path).await {
                Ok(meta) => {
                    error_streak = 0;
                    let size = meta.len();
                    if size < offset {
                        offset = 0;
                        leftover.clear();
                    }
                    if size > offset {
                        use tokio::io::{AsyncReadExt, AsyncSeekExt};
                        if let Ok(mut f) = tokio::fs::File::open(&log_path).await {
                            if f.seek(std::io::SeekFrom::Start(offset)).await.is_ok() {
                                let mut buf = Vec::new();
                                if f.read_to_end(&mut buf).await.is_ok() {
                                    offset = size;
                                    let chunk = String::from_utf8_lossy(&buf);
                                    leftover.push_str(&chunk);
                                    let mut parts: Vec<&str> = leftover.split('\n').collect();
                                    let trail = parts.pop().unwrap_or("").to_string();
                                    for line in parts {
                                        let cleaned = strip_ansi(line);
                                        if cleaned.is_empty() {
                                            continue;
                                        }
                                        if tx.send((session_id.clone(), cleaned)).await.is_err() {
                                            return;
                                        }
                                    }
                                    leftover = trail;
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    error_streak += 1;
                    if error_streak >= 10 {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
}

/// Capture the current pane content (snapshot).
pub async fn capture_pane(name: &str) -> Result<String> {
    run_tmux(&["capture-pane", "-t", name, "-p"]).await
}

/// Snapshot the visible pane, return as one String per row (trailing blank
/// rows trimmed). This is what the user would see if they attached.
pub async fn capture_pane_lines(name: &str) -> Result<Vec<String>> {
    let raw = capture_pane(name).await?;
    let mut lines: Vec<String> = raw.lines().map(|l| l.trim_end().to_string()).collect();
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    Ok(lines)
}

/// Spawn a task that periodically captures the visible pane and pushes a
/// fresh snapshot to the channel. Each event is the entire current view —
/// the receiver should REPLACE its buffer, not append. This avoids the
/// noise of pipe-pane (status-bar redraws, cursor escapes, etc.) and gives
/// the UI the same view the user would see by attaching.
pub fn spawn_pane_capturer(
    session_id: String,
    tmux_session: String,
    tx: mpsc::Sender<(String, Vec<String>)>,
    interval: Duration,
) {
    tokio::spawn(async move {
        loop {
            if tx.is_closed() {
                return;
            }
            if !has_session(&tmux_session).await {
                // tmux session is gone — stop capturing.
                return;
            }
            if let Ok(lines) = capture_pane_lines(&tmux_session).await {
                if tx.send((session_id.clone(), lines)).await.is_err() {
                    return;
                }
            }
            tokio::time::sleep(interval).await;
        }
    });
}

/// Attach to a tmux session (blocking — user takes over terminal).
/// Clears `TMUX` env to allow nesting (orc runs inside the user's tmux).
pub async fn attach(name: &str) -> Result<()> {
    let status = std::process::Command::new("tmux")
        .env_remove("TMUX")
        .args(["attach-session", "-t", name])
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux attach failed");
    }
    Ok(())
}

/// List all orc-related tmux sessions.
pub async fn list_orc_sessions() -> Result<Vec<String>> {
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .await?;
    if !output.status.success() {
        // No sessions at all — tmux exits non-zero when there are no sessions.
        return Ok(vec![]);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let sessions = stdout
        .lines()
        .filter(|l| l.starts_with("orc-"))
        .map(|l| l.to_string())
        .collect();
    Ok(sessions)
}

/// Clean up orphaned orc sessions.
pub async fn cleanup_orphaned() -> Result<usize> {
    let sessions = list_orc_sessions().await?;
    let count = sessions.len();
    for name in &sessions {
        // Best-effort; ignore individual failures.
        let _ = kill_session(name).await;
    }
    Ok(count)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    // ── unit tests (no tmux required) ────────────────────────────────────────

    #[test]
    fn tmux_conf_has_required_lines() {
        let conf = tmux_conf_content();
        assert!(conf.contains("set -g status off"));
        assert!(conf.contains("set -g prefix None"));
        assert!(conf.contains("unbind C-b"));
        assert!(conf.contains("bind -n C-q detach-client"));
        assert!(conf.contains("set -g mouse off"));
        assert!(conf.contains("set -g default-terminal \"screen-256color\""));
    }

    #[tokio::test]
    async fn conf_write_and_read_back() {
        let name = "test-unit";
        let path = write_conf(name).await.expect("write_conf failed");
        let content = fs::read_to_string(&path).await.expect("read back failed");
        assert_eq!(content, tmux_conf_content());
        let _ = fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn conf_path_uses_temp_dir_and_name() {
        let p = conf_path("myworker");
        assert!(p.starts_with(std::env::temp_dir()));
        assert!(p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("myworker"));
    }

    #[test]
    fn strip_ansi_removes_csi() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m text"), "red text");
        assert_eq!(strip_ansi("\x1b[2;5Hcursor"), "cursor");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn strip_ansi_removes_osc() {
        assert_eq!(strip_ansi("\x1b]0;title\x07hello"), "hello");
    }

    #[test]
    fn strip_ansi_removes_cr_and_controls() {
        assert_eq!(strip_ansi("a\rb"), "ab");
        assert_eq!(strip_ansi("a\x08b"), "ab");
        assert_eq!(strip_ansi("a\tb"), "a\tb"); // tab kept
    }

    #[tokio::test]
    async fn wait_for_log_match_finds_marker() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ready.log");
        std::fs::File::create(&path).unwrap();

        // Write the marker after a short delay from a separate task.
        let path_w = path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path_w)
                .unwrap();
            // Include an ANSI escape to ensure stripping is in play.
            writeln!(f, "\x1b[1mClaude Code v2.1.123\x1b[0m").unwrap();
        });

        let found = wait_for_log_match(&path, &["Claude Code v"], Duration::from_secs(3))
            .await
            .unwrap();
        assert!(found, "expected marker to be found");
    }

    #[tokio::test]
    async fn wait_for_log_match_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("never.log");
        std::fs::File::create(&path).unwrap();
        let found = wait_for_log_match(&path, &["NEVER_APPEARS"], Duration::from_millis(400))
            .await
            .unwrap();
        assert!(!found, "expected timeout");
    }

    #[tokio::test]
    async fn wait_for_log_match_handles_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-yet.log");
        // File doesn't exist yet — wait_for_log_match should keep polling, not error.
        let path_w = path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            std::fs::write(&path_w, "ready: ❯ prompt").unwrap();
        });
        let found = wait_for_log_match(&path, &["❯"], Duration::from_secs(3))
            .await
            .unwrap();
        assert!(found);
    }

    #[tokio::test]
    async fn log_tailer_streams_new_lines() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tail.log");
        std::fs::File::create(&path).unwrap();

        let (tx, mut rx) = mpsc::channel::<(String, String)>(64);
        spawn_log_tailer("s1".to_string(), path.clone(), tx);

        // give the tailer a moment to attach
        tokio::time::sleep(Duration::from_millis(150)).await;

        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "\x1b[31mfirst\x1b[0m").unwrap();
        writeln!(f, "second").unwrap();
        drop(f);

        let mut got = Vec::new();
        for _ in 0..2 {
            match tokio::time::timeout(Duration::from_secs(3), rx.recv()).await {
                Ok(Some((id, line))) => {
                    assert_eq!(id, "s1");
                    got.push(line);
                }
                _ => break,
            }
        }
        assert_eq!(got, vec!["first".to_string(), "second".to_string()]);
    }

    #[tokio::test]
    async fn read_pipe_log_returns_last_n() {
        let mut tmp = NamedTempFile::new().unwrap();
        use std::io::Write;
        for i in 0..10 {
            writeln!(tmp, "line {i}").unwrap();
        }
        let lines = read_pipe_log(tmp.path(), 3).await.unwrap();
        assert_eq!(lines, vec!["line 7", "line 8", "line 9"]);
    }

    #[tokio::test]
    async fn read_pipe_log_fewer_lines_than_n() {
        let mut tmp = NamedTempFile::new().unwrap();
        use std::io::Write;
        writeln!(tmp, "only line").unwrap();
        let lines = read_pipe_log(tmp.path(), 100).await.unwrap();
        assert_eq!(lines, vec!["only line"]);
    }

    #[tokio::test]
    async fn read_pipe_log_empty_file() {
        let tmp = NamedTempFile::new().unwrap();
        let lines = read_pipe_log(tmp.path(), 5).await.unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn list_orc_sessions_filter_logic() {
        // Simulate the filter used in list_orc_sessions
        let raw = "orc-worker1\nsome-other\norc-worker2\nmain\n";
        let filtered: Vec<String> = raw
            .lines()
            .filter(|l| l.starts_with("orc-"))
            .map(|l| l.to_string())
            .collect();
        assert_eq!(filtered, vec!["orc-worker1", "orc-worker2"]);
    }

    // ── integration tests (require tmux) ───────────────────────────────────
    // Session names use "tst-" prefix (not "orc-") so cleanup_orphaned() won't
    // race-kill sessions owned by other parallel tests.

    #[tokio::test]
    async fn integration_create_and_kill_session() {
        let name = "tst-create-kill";
        let dir = std::env::temp_dir();
        let _ = kill_session(name).await; // ensure clean slate
        assert!(!has_session(name).await, "session should not exist yet");
        create_session(name, &dir).await.expect("create_session");
        assert!(has_session(name).await, "session should exist after create");
        kill_session(name).await.expect("kill_session");
        assert!(!has_session(name).await, "session should be gone after kill");
    }

    #[tokio::test]
    async fn integration_send_text_and_capture() {
        let name = "tst-send-capture";
        let dir = std::env::temp_dir();
        let _ = kill_session(name).await;
        create_session(name, &dir).await.expect("create_session");
        send_keys(name, &["echo hello-orc-test", "Enter"])
            .await
            .expect("send_keys");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let pane = capture_pane(name).await.expect("capture_pane");
        assert!(pane.contains("hello-orc-test"), "pane: {pane}");
        kill_session(name).await.expect("kill_session");
    }

    #[tokio::test]
    async fn integration_list_and_cleanup_orc_sessions() {
        // These use "orc-" prefix intentionally — they're the ones cleanup targets.
        let names = ["orc-cleanup-a", "orc-cleanup-b"];
        let dir = std::env::temp_dir();
        for n in &names {
            let _ = kill_session(n).await;
            create_session(n, &dir).await.expect("create_session");
        }
        let sessions = list_orc_sessions().await.expect("list_orc_sessions");
        for n in &names {
            assert!(sessions.iter().any(|s| s == n), "missing {n}");
        }
        let killed = cleanup_orphaned().await.expect("cleanup_orphaned");
        assert!(killed >= 2, "expected at least 2 killed, got {killed}");
        for n in &names {
            assert!(!has_session(n).await, "{n} should be gone");
        }
    }

    #[tokio::test]
    async fn integration_pipe_pane_log() {
        let name = "tst-pipe-pane";
        let dir = std::env::temp_dir();
        let logfile = dir.join("tst-pipe-pane.log");
        let _ = kill_session(name).await;
        let _ = fs::remove_file(&logfile).await;
        create_session(name, &dir).await.expect("create_session");
        start_pipe_pane(name, &logfile).await.expect("start_pipe_pane");
        send_keys(name, &["echo pipe-test-output", "Enter"])
            .await
            .expect("send_keys");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let lines = read_pipe_log(&logfile, 10).await.expect("read_pipe_log");
        assert!(
            lines.iter().any(|l| l.contains("pipe-test-output")),
            "lines: {lines:?}"
        );
        kill_session(name).await.expect("kill_session");
        let _ = fs::remove_file(&logfile).await;
    }
}
