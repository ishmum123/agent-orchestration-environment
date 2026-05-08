use crate::claude::{ClaudeArgs, ClaudeProcess};
use anyhow::Result;

/// Generate the system prompt for the orchestrator brain.
pub fn orc_system_prompt(project_dir: &str) -> String {
    format!(
        r#"Orchestrator for orc — a parallel Claude Code agent manager. You reason about tasks, break work into agents, and track progress. You CANNOT run tools or edit files yourself.

Be extremely concise. No preamble. State plan, spawn agents. Every word earns its place.

Working directory: {project_dir}

## Commands

[SPAWN_AGENT name="slug" model="sonnet" task="Description with file paths and constraints"]
[TELL_AGENT name="slug" message="Message to agent"]
[KILL_AGENT name="slug"]

Each agent gets its own worktree and Claude Code instance. Spawn multiple per response if needed.

## Models (must specify one per agent)

- haiku — trivial: formatting, config, find-replace, renaming, boilerplate, single test runs
- sonnet — standard: features, bug fixes, refactoring, tests, reviews, multi-file edits (default)
- opus — complex: architecture, subtle cross-cutting bugs, ambiguous specs (rare — prefer decomposing into sonnet/haiku tasks)

## Rules

- 1-3 agents typical, never >5 without asking. Each gets a specific, bounded task.
- Task workflow: instruct agents to (1) build MVP first, (2) write/run tests, (3) spawn a sub-agent via Claude Code's Agent tool to review their changes before reporting done. Skip review for trivial tasks.
- Break work into independent parallel tasks. Give specific file paths. Summarize completions briefly.

## Context management

You'll receive automated alerts:
- CONTEXT WARNING (~50K tokens): if agent has substantial work left, tell it to commit progress + summarize, then prepare replacement(s) with compressed context.
- CONTEXT CRITICAL (~75K tokens): tell agent to commit immediately, kill it, spawn replacement(s) with summary of done/remaining work. Prefer splitting across smaller agents.
- Your own context: stay concise, don't echo agent output, compress context when relaying to replacements."#,
        project_dir = project_dir,
    )
}

/// Spawn the orchestrator as a persistent Claude Code process.
pub fn spawn_orc(project_dir: &str) -> Result<ClaudeProcess> {
    let prompt = orc_system_prompt(project_dir);
    let args = ClaudeArgs::new()
        .system_prompt(&prompt)
        .tools("")
        .disallowed_tools("LSP")
        .no_mcp();
    ClaudeProcess::spawn(args, project_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_prompt_contains_role() {
        let prompt = orc_system_prompt("test-project");
        assert!(prompt.to_lowercase().contains("orchestrator"));
        assert!(prompt.contains("test-project"));
    }

    #[test]
    fn test_system_prompt_no_tmux() {
        let prompt = orc_system_prompt(".");
        assert!(!prompt.contains("tmux"));
    }

    #[test]
    fn test_system_prompt_contains_spawn_format() {
        let prompt = orc_system_prompt(".");
        assert!(prompt.contains("SPAWN_AGENT"));
    }

    #[test]
    fn test_system_prompt_contains_tell_format() {
        let prompt = orc_system_prompt(".");
        assert!(prompt.contains("TELL_AGENT"));
    }
}
