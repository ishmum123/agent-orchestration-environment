use anyhow::{Context, Result};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct TmuxPane {
    pub session: String,
    pub window: u32,
    pub pane: u32,
}

impl TmuxPane {
    pub fn target(&self) -> String {
        format!("{}:{}.{}", self.session, self.window, self.pane)
    }
}

pub struct TmuxSession {
    pub name: String,
}

impl TmuxSession {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }

    pub fn create(&self) -> Result<()> {
        Command::new("tmux")
            .args(["new-session", "-d", "-s", &self.name, "-x", "200", "-y", "50"])
            .status()
            .context(format!("failed to create tmux session '{}'", self.name))?
            .success()
            .then_some(())
            .context(format!("tmux new-session failed for '{}'", self.name))
    }

    pub fn kill(&self) -> Result<()> {
        Command::new("tmux")
            .args(["kill-session", "-t", &self.name])
            .status()
            .context(format!("failed to kill tmux session '{}'", self.name))?
            .success()
            .then_some(())
            .context(format!("tmux kill-session failed for '{}'", self.name))
    }

    pub fn exists(&self) -> bool {
        Command::new("tmux")
            .args(["has-session", "-t", &self.name])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn create_pane(&self, working_dir: &str) -> Result<TmuxPane> {
        let output = Command::new("tmux")
            .args([
                "split-window",
                "-t",
                &self.name,
                "-c",
                working_dir,
                "-d",
                "-P",
                "-F",
                "#{pane_index}",
            ])
            .output()
            .context(format!(
                "failed to create pane in session '{}'",
                self.name
            ))?;

        anyhow::ensure!(
            output.status.success(),
            "tmux split-window failed for session '{}'",
            self.name
        );

        let index_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let pane_index: u32 = index_str
            .parse()
            .context(format!("failed to parse pane index '{}'", index_str))?;

        Ok(TmuxPane {
            session: self.name.clone(),
            window: 0,
            pane: pane_index,
        })
    }

    pub fn kill_pane(&self, pane: &TmuxPane) -> Result<()> {
        Command::new("tmux")
            .args(["kill-pane", "-t", &pane.target()])
            .status()
            .context(format!("failed to kill pane '{}'", pane.target()))?
            .success()
            .then_some(())
            .context(format!("tmux kill-pane failed for '{}'", pane.target()))
    }

    pub fn attach(&self) -> Result<()> {
        Command::new("tmux")
            .args(["attach-session", "-t", &self.name])
            .status()
            .context(format!("failed to attach to session '{}'", self.name))?
            .success()
            .then_some(())
            .context(format!(
                "tmux attach-session failed for '{}'",
                self.name
            ))
    }

    pub fn select_pane(&self, pane: &TmuxPane) -> Result<()> {
        Command::new("tmux")
            .args(["select-pane", "-t", &pane.target()])
            .status()
            .context(format!("failed to select pane '{}'", pane.target()))?
            .success()
            .then_some(())
            .context(format!("tmux select-pane failed for '{}'", pane.target()))
    }

    pub fn pane_count(&self) -> Result<u32> {
        let target = format!("{}:0", self.name);
        let output = Command::new("tmux")
            .args(["list-panes", "-t", &target])
            .output()
            .context(format!(
                "failed to list panes in session '{}'",
                self.name
            ))?;

        anyhow::ensure!(
            output.status.success(),
            "tmux list-panes failed for session '{}'",
            self.name
        );

        let count = String::from_utf8_lossy(&output.stdout)
            .lines()
            .count() as u32;
        Ok(count)
    }
}

pub fn send_keys(pane: &TmuxPane, text: &str) -> Result<()> {
    Command::new("tmux")
        .args(["send-keys", "-t", &pane.target(), text, "Enter"])
        .status()
        .context(format!("failed to send keys to pane '{}'", pane.target()))?
        .success()
        .then_some(())
        .context(format!(
            "tmux send-keys failed for pane '{}'",
            pane.target()
        ))
}

pub fn send_keys_raw(pane: &TmuxPane, text: &str) -> Result<()> {
    Command::new("tmux")
        .args(["send-keys", "-t", &pane.target(), text])
        .status()
        .context(format!(
            "failed to send raw keys to pane '{}'",
            pane.target()
        ))?
        .success()
        .then_some(())
        .context(format!(
            "tmux send-keys (raw) failed for pane '{}'",
            pane.target()
        ))
}

pub fn capture_pane(pane: &TmuxPane, lines: i32) -> Result<String> {
    let start = format!("-{}", lines);
    let output = Command::new("tmux")
        .args([
            "capture-pane",
            "-t",
            &pane.target(),
            "-p",
            "-J",
            "-S",
            &start,
        ])
        .output()
        .context(format!(
            "failed to capture pane '{}'",
            pane.target()
        ))?;

    anyhow::ensure!(
        output.status.success(),
        "tmux capture-pane failed for pane '{}'",
        pane.target()
    );

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
