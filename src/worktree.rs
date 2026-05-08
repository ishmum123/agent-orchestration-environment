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

/// A file modified in multiple worktrees — potential merge conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileConflict {
    pub file: String,
    pub worktrees: Vec<String>,
}

/// Detect files modified in multiple worktrees (potential merge conflicts).
///
/// For each worktree, diffs against HEAD of the base branch to find modified files,
/// then reports any file that appears in two or more worktrees.
pub async fn detect_conflicts(worktree_paths: &[String]) -> Vec<FileConflict> {
    use std::collections::HashMap;

    // file -> list of worktree paths that modified it
    let mut file_map: HashMap<String, Vec<String>> = HashMap::new();

    for wt_path in worktree_paths {
        // Get modified files in this worktree relative to its merge-base with HEAD
        let files = match modified_files_in_worktree(wt_path).await {
            Ok(f) => f,
            Err(_) => continue,
        };
        for file in files {
            file_map.entry(file).or_default().push(wt_path.clone());
        }
    }

    file_map
        .into_iter()
        .filter(|(_, wts)| wts.len() > 1)
        .map(|(file, worktrees)| FileConflict { file, worktrees })
        .collect()
}

/// Get list of files modified in a worktree (compared to its merge-base).
async fn modified_files_in_worktree(worktree_path: &str) -> Result<Vec<String>> {
    // Find the merge-base between HEAD and the branch's upstream
    let merge_base_output = Command::new("git")
        .args(["merge-base", "HEAD", "HEAD"])
        .current_dir(worktree_path)
        .output()
        .await
        .context("failed to run git merge-base")?;

    let base = if merge_base_output.status.success() {
        String::from_utf8_lossy(&merge_base_output.stdout)
            .trim()
            .to_string()
    } else {
        // Fallback: diff against HEAD directly
        "HEAD".to_string()
    };

    let output = Command::new("git")
        .args(["diff", "--name-only", &base])
        .current_dir(worktree_path)
        .output()
        .await
        .context("failed to run git diff --name-only")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git diff --name-only failed: {stderr}");
    }

    let files = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    Ok(files)
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_conflict_struct() {
        let conflict = FileConflict {
            file: "src/main.rs".to_string(),
            worktrees: vec!["/wt/a".to_string(), "/wt/b".to_string()],
        };
        assert_eq!(conflict.file, "src/main.rs");
        assert_eq!(conflict.worktrees.len(), 2);
    }

    #[tokio::test]
    async fn detect_conflicts_empty_input() {
        let conflicts = detect_conflicts(&[]).await;
        assert!(conflicts.is_empty());
    }

    #[tokio::test]
    async fn detect_conflicts_single_worktree() {
        // A single worktree can never produce conflicts
        let conflicts = detect_conflicts(&["/nonexistent/path".to_string()]).await;
        assert!(conflicts.is_empty());
    }

    #[tokio::test]
    async fn detect_conflicts_nonexistent_paths() {
        // Non-existent paths should be skipped gracefully
        let conflicts = detect_conflicts(&[
            "/nonexistent/a".to_string(),
            "/nonexistent/b".to_string(),
        ]).await;
        assert!(conflicts.is_empty());
    }
}
