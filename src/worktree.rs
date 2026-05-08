// Git worktree management: create/remove isolated worktrees for agents.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Returns the git repo root for the given project directory.
pub async fn repo_root(project_dir: &str) -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(project_dir)
        .output()
        .await
        .context("failed to run git rev-parse --show-toplevel")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git rev-parse --show-toplevel failed: {stderr}");
    }

    let root = String::from_utf8(output.stdout)
        .context("non-utf8 repo root path")?
        .trim()
        .to_string();

    Ok(PathBuf::from(root))
}

/// Detects the main branch name ("main" or "master").
pub async fn main_branch(project_dir: &str) -> Result<String> {
    let check = |branch: &'static str| {
        let dir = project_dir.to_string();
        async move {
            Command::new("git")
                .args(["rev-parse", "--verify", branch])
                .current_dir(&dir)
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false)
        }
    };

    if check("main").await {
        Ok("main".to_string())
    } else if check("master").await {
        Ok("master".to_string())
    } else {
        bail!("neither 'main' nor 'master' branch exists")
    }
}

/// Creates a git worktree for the given agent.
///
/// Branch: `orc/{agent_name}`
/// Directory: `{repo_root_parent}/.orc-worktrees/{agent_name}`
pub async fn create_worktree(project_dir: &str, agent_name: &str) -> Result<PathBuf> {
    let root = repo_root(project_dir).await?;
    let base = main_branch(project_dir).await?;

    let branch = format!("orc/{agent_name}");
    let worktree_dir = root
        .parent()
        .unwrap_or(Path::new("/"))
        .join(".orc-worktrees")
        .join(agent_name);

    tokio::fs::create_dir_all(worktree_dir.parent().unwrap())
        .await
        .context("failed to create worktree parent directory")?;

    let output = Command::new("git")
        .args([
            "worktree",
            "add",
            "-b",
            &branch,
            worktree_dir
                .to_str()
                .context("worktree path not valid UTF-8")?,
            &base,
        ])
        .current_dir(project_dir)
        .output()
        .await
        .context("failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree add failed: {stderr}");
    }

    Ok(worktree_dir)
}

/// Removes the worktree and branch for the given agent.
pub async fn remove_worktree(
    project_dir: &str,
    worktree_path: &Path,
    agent_name: &str,
) -> Result<()> {
    let worktree_str = worktree_path
        .to_str()
        .context("worktree path not valid UTF-8")?;

    let output = Command::new("git")
        .args(["worktree", "remove", "--force", worktree_str])
        .current_dir(project_dir)
        .output()
        .await
        .context("failed to run git worktree remove")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree remove failed: {stderr}");
    }

    let branch = format!("orc/{agent_name}");
    let output = Command::new("git")
        .args(["branch", "-D", &branch])
        .current_dir(project_dir)
        .output()
        .await
        .context("failed to run git branch -D")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git branch -D {branch} failed: {stderr}");
    }

    Ok(())
}
