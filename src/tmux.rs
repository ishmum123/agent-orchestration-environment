// Tmux session lifecycle: create, attach, kill worker panes.

use anyhow::Result;
use std::path::Path;
use tokio::fs;
use tokio::process::Command;

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
     bind C-q detach-client\n\
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

/// Capture the current pane content (snapshot).
pub async fn capture_pane(name: &str) -> Result<String> {
    run_tmux(&["capture-pane", "-t", name, "-p"]).await
}

/// Attach to a tmux session (blocking — user takes over terminal).
pub async fn attach(name: &str) -> Result<()> {
    let status = std::process::Command::new("tmux")
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
        assert!(conf.contains("bind C-q detach-client"));
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

    // ── integration tests (require tmux, run manually with `cargo test -- --ignored`) ──

    #[tokio::test]
    #[ignore]
    async fn integration_create_and_kill_session() {
        let name = "orc-test-integration";
        let dir = std::env::temp_dir();
        assert!(!has_session(name).await, "session should not exist yet");
        create_session(name, &dir).await.expect("create_session");
        assert!(has_session(name).await, "session should exist after create");
        kill_session(name).await.expect("kill_session");
        assert!(!has_session(name).await, "session should be gone after kill");
    }

    #[tokio::test]
    #[ignore]
    async fn integration_send_text_and_capture() {
        let name = "orc-test-send";
        let dir = std::env::temp_dir();
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
    #[ignore]
    async fn integration_list_and_cleanup_orc_sessions() {
        let names = ["orc-cleanup-a", "orc-cleanup-b"];
        let dir = std::env::temp_dir();
        for n in &names {
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
    #[ignore]
    async fn integration_pipe_pane_log() {
        let name = "orc-test-pipe";
        let dir = std::env::temp_dir();
        let logfile = dir.join("orc-test-pipe.log");
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
