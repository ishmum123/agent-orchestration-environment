// Policy engine: parse policy.toml, match rules against agent actions.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny,
    HardDeny, // coded constants, cannot be overridden
    Escalate, // pass to orc for decision
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub tool_name: String,
    pub action: String,       // e.g. "read", "write", "execute", "network"
    pub target: String,       // e.g. file path, command, URL
    pub session_id: String,
    pub worktree_path: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PolicyConfig {
    #[serde(default)]
    pub allow: PolicyRules,
    #[serde(default)]
    pub deny: PolicyRules,
    #[serde(default)]
    pub escalate: PolicyRules,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PolicyRules {
    #[serde(default)]
    pub read_in_repo: bool,
    #[serde(default)]
    pub write_in_worktree: bool,
    #[serde(default)]
    pub run_tests: bool,
    #[serde(default)]
    pub run_lint: bool,
    #[serde(default)]
    pub write_outside_worktree: bool,
    #[serde(default)]
    pub network_calls: bool,
    #[serde(default)]
    pub package_install: bool,
    #[serde(default)]
    pub modify_secrets: bool,
}

pub struct PolicyEngine {
    config: PolicyConfig,
}

impl PolicyEngine {
    /// Load policy from a TOML file. If file doesn't exist, use defaults.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default_policy());
        }
        let contents = std::fs::read_to_string(path)?;
        Self::from_str(&contents)
    }

    /// Load from a TOML string (useful for testing).
    pub fn from_str(toml_str: &str) -> Result<Self> {
        let config: PolicyConfig = toml::from_str(toml_str)?;
        Ok(Self { config })
    }

    /// Default policy (generous defaults for development).
    pub fn default_policy() -> Self {
        let config = PolicyConfig {
            allow: PolicyRules {
                read_in_repo: true,
                write_in_worktree: true,
                run_tests: true,
                run_lint: true,
                ..Default::default()
            },
            escalate: PolicyRules {
                write_outside_worktree: true,
                network_calls: true,
                package_install: true,
                ..Default::default()
            },
            deny: PolicyRules {
                modify_secrets: true,
                ..Default::default()
            },
        };
        Self { config }
    }

    /// Evaluate a permission request.
    /// Check order: hard-deny first, then deny rules, then allow rules, then escalate rules.
    /// If no rule matches, default to Escalate.
    pub fn evaluate(&self, request: &PermissionRequest) -> PolicyDecision {
        if is_hard_deny(request) {
            return PolicyDecision::HardDeny;
        }

        let category = classify_request(request);

        if rule_matches(&self.config.deny, category) {
            return PolicyDecision::Deny;
        }

        if rule_matches(&self.config.allow, category) {
            return PolicyDecision::Allow;
        }

        if rule_matches(&self.config.escalate, category) {
            return PolicyDecision::Escalate;
        }

        PolicyDecision::Escalate
    }
}

fn rule_matches(rules: &PolicyRules, category: &str) -> bool {
    match category {
        "read_in_repo" => rules.read_in_repo,
        "write_in_worktree" => rules.write_in_worktree,
        "write_outside_worktree" => rules.write_outside_worktree,
        "run_tests" => rules.run_tests,
        "run_lint" => rules.run_lint,
        "package_install" => rules.package_install,
        "network_calls" => rules.network_calls,
        "modify_secrets" => rules.modify_secrets,
        _ => false,
    }
}

fn is_hard_deny(request: &PermissionRequest) -> bool {
    if request.action == "execute" && is_force_push(&request.target) {
        return true;
    }
    if request.action == "execute" && is_destructive_main_op(&request.target) {
        return true;
    }
    if request.action == "write" && is_system_path(&request.target) {
        return true;
    }
    false
}

fn is_force_push(cmd: &str) -> bool {
    cmd.contains("push") && (cmd.contains("--force") || cmd.contains(" -f"))
}

fn is_destructive_main_op(cmd: &str) -> bool {
    let targets_main = cmd.contains("main") || cmd.contains("master");
    let destructive = cmd.contains("rm ")
        || cmd.contains("reset --hard")
        || (cmd.contains("push") && (cmd.contains("--force") || cmd.contains(" -f")));
    targets_main && destructive
}

fn is_system_path(path: &str) -> bool {
    path.starts_with("/etc/")
        || path.starts_with("/usr/")
        || path.starts_with("/System/")
        || path.contains("/.ssh/")
}

fn classify_request(request: &PermissionRequest) -> &str {
    match request.action.as_str() {
        "read" if is_in_repo(&request.target, &request.worktree_path) => "read_in_repo",
        "write" if is_secret_file(&request.target) => "modify_secrets",
        "write" if is_in_worktree(&request.target, &request.worktree_path) => "write_in_worktree",
        "write" => "write_outside_worktree",
        "execute" if is_test_command(&request.target) => "run_tests",
        "execute" if is_lint_command(&request.target) => "run_lint",
        "execute" if is_package_install(&request.target) => "package_install",
        "network" => "network_calls",
        _ => "unknown",
    }
}

fn is_in_repo(target: &str, worktree_path: &str) -> bool {
    // The repo root is the parent of the worktree or the worktree itself.
    // A simple heuristic: the target starts with the worktree path or any parent path segment.
    // For robustness, check if target is under the worktree or its parent.
    if target.starts_with(worktree_path) {
        return true;
    }
    // Try parent of worktree (repo root)
    if let Some(parent) = std::path::Path::new(worktree_path).parent() {
        let parent_str = parent.to_string_lossy();
        if !parent_str.is_empty() && target.starts_with(parent_str.as_ref()) {
            return true;
        }
    }
    false
}

fn is_in_worktree(target: &str, worktree_path: &str) -> bool {
    target.starts_with(worktree_path)
}

fn is_test_command(cmd: &str) -> bool {
    cmd.contains("cargo test")
        || cmd.contains("npm test")
        || cmd.contains("npm run test")
        || cmd.contains("pytest")
        || cmd.contains("go test")
        || cmd.contains("jest")
        || cmd.contains("yarn test")
}

fn is_lint_command(cmd: &str) -> bool {
    cmd.contains("cargo clippy")
        || cmd.contains("eslint")
        || cmd.contains("flake8")
        || cmd.contains("pylint")
        || cmd.contains("cargo fmt")
        || cmd.contains("rustfmt")
        || cmd.contains("golangci-lint")
}

fn is_package_install(cmd: &str) -> bool {
    cmd.contains("cargo add")
        || cmd.contains("npm install")
        || cmd.contains("pip install")
        || cmd.contains("yarn add")
        || cmd.contains("gem install")
        || cmd.contains("go get")
        || cmd.contains("apt install")
        || cmd.contains("brew install")
}

fn is_secret_file(path: &str) -> bool {
    path.contains(".env")
        || path.contains("credentials")
        || path.contains("secret")
        || path.contains(".key")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_request(action: &str, target: &str) -> PermissionRequest {
        PermissionRequest {
            tool_name: "Bash".into(),
            action: action.into(),
            target: target.into(),
            session_id: "test-session".into(),
            worktree_path: "/repo/worktrees/agent-1".into(),
        }
    }

    fn make_request_in_worktree(action: &str, target: &str) -> PermissionRequest {
        PermissionRequest {
            tool_name: "Write".into(),
            action: action.into(),
            target: target.into(),
            session_id: "test-session".into(),
            worktree_path: "/repo/worktrees/agent-1".into(),
        }
    }

    #[test]
    fn hard_deny_force_push() {
        let engine = PolicyEngine::default_policy();
        let req = make_request("execute", "git push --force origin main");
        assert_eq!(engine.evaluate(&req), PolicyDecision::HardDeny);
    }

    #[test]
    fn hard_deny_force_push_short_flag() {
        let engine = PolicyEngine::default_policy();
        let req = make_request("execute", "git push -f origin main");
        assert_eq!(engine.evaluate(&req), PolicyDecision::HardDeny);
    }

    #[test]
    fn hard_deny_overrides_allow_policy() {
        // Even if we configure allow for execute, hard-deny wins
        let toml = r#"
[allow]
run_tests = true
write_in_worktree = true
"#;
        let engine = PolicyEngine::from_str(toml).unwrap();
        let req = make_request("execute", "git push --force");
        assert_eq!(engine.evaluate(&req), PolicyDecision::HardDeny);
    }

    #[test]
    fn hard_deny_system_paths() {
        let engine = PolicyEngine::default_policy();
        let req = make_request("write", "/etc/hosts");
        assert_eq!(engine.evaluate(&req), PolicyDecision::HardDeny);

        let req2 = make_request("write", "/usr/local/bin/tool");
        assert_eq!(engine.evaluate(&req2), PolicyDecision::HardDeny);

        let req3 = make_request("write", "/home/user/.ssh/id_rsa");
        assert_eq!(engine.evaluate(&req3), PolicyDecision::HardDeny);
    }

    #[test]
    fn hard_deny_destructive_main() {
        let engine = PolicyEngine::default_policy();
        let req = make_request("execute", "git reset --hard main");
        assert_eq!(engine.evaluate(&req), PolicyDecision::HardDeny);

        let req2 = make_request("execute", "rm -rf main");
        assert_eq!(engine.evaluate(&req2), PolicyDecision::HardDeny);
    }

    #[test]
    fn read_in_repo_allowed() {
        let engine = PolicyEngine::default_policy();
        // target is inside worktree (which is inside repo)
        let req = make_request("read", "/repo/worktrees/agent-1/src/main.rs");
        assert_eq!(engine.evaluate(&req), PolicyDecision::Allow);
    }

    #[test]
    fn write_in_worktree_allowed() {
        let engine = PolicyEngine::default_policy();
        let req = make_request_in_worktree("write", "/repo/worktrees/agent-1/src/new_file.rs");
        assert_eq!(engine.evaluate(&req), PolicyDecision::Allow);
    }

    #[test]
    fn write_outside_worktree_escalates() {
        let engine = PolicyEngine::default_policy();
        let req = make_request("write", "/home/user/some_other_file.txt");
        assert_eq!(engine.evaluate(&req), PolicyDecision::Escalate);
    }

    #[test]
    fn run_tests_allowed() {
        let engine = PolicyEngine::default_policy();
        let req = make_request("execute", "cargo test --all");
        assert_eq!(engine.evaluate(&req), PolicyDecision::Allow);

        let req2 = make_request("execute", "pytest tests/");
        assert_eq!(engine.evaluate(&req2), PolicyDecision::Allow);
    }

    #[test]
    fn network_calls_escalate() {
        let engine = PolicyEngine::default_policy();
        let req = PermissionRequest {
            tool_name: "WebFetch".into(),
            action: "network".into(),
            target: "https://example.com/api".into(),
            session_id: "test".into(),
            worktree_path: "/repo/worktrees/agent-1".into(),
        };
        assert_eq!(engine.evaluate(&req), PolicyDecision::Escalate);
    }

    #[test]
    fn modify_secrets_denied() {
        let engine = PolicyEngine::default_policy();
        let req = make_request("write", "/repo/worktrees/agent-1/.env");
        assert_eq!(engine.evaluate(&req), PolicyDecision::Deny);

        let req2 = make_request("write", "/repo/credentials.json");
        assert_eq!(engine.evaluate(&req2), PolicyDecision::Deny);
    }

    #[test]
    fn unknown_action_escalates() {
        let engine = PolicyEngine::default_policy();
        let req = PermissionRequest {
            tool_name: "Unknown".into(),
            action: "something_new".into(),
            target: "target".into(),
            session_id: "test".into(),
            worktree_path: "/repo/worktrees/agent-1".into(),
        };
        assert_eq!(engine.evaluate(&req), PolicyDecision::Escalate);
    }

    #[test]
    fn default_policy_permissive_for_dev() {
        let engine = PolicyEngine::default_policy();

        // These should all be allowed
        assert_eq!(
            engine.evaluate(&make_request("read", "/repo/worktrees/agent-1/src/lib.rs")),
            PolicyDecision::Allow
        );
        assert_eq!(
            engine.evaluate(&make_request("write", "/repo/worktrees/agent-1/output.txt")),
            PolicyDecision::Allow
        );
        assert_eq!(
            engine.evaluate(&make_request("execute", "cargo test")),
            PolicyDecision::Allow
        );
        assert_eq!(
            engine.evaluate(&make_request("execute", "cargo clippy")),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn custom_policy_from_toml() {
        let toml = r#"
[allow]
read_in_repo = true
write_in_worktree = true
run_tests = true
run_lint = true

[escalate]
write_outside_worktree = true
network_calls = true
package_install = true

[deny]
modify_secrets = true
"#;
        let engine = PolicyEngine::from_str(toml).unwrap();

        assert_eq!(
            engine.evaluate(&make_request("read", "/repo/worktrees/agent-1/file.rs")),
            PolicyDecision::Allow
        );
        assert_eq!(
            engine.evaluate(&make_request("write", "/repo/worktrees/agent-1/file.rs")),
            PolicyDecision::Allow
        );
        assert_eq!(
            engine.evaluate(&make_request("write", "/tmp/outside.txt")),
            PolicyDecision::Escalate
        );
        assert_eq!(
            engine.evaluate(&make_request("execute", "npm install lodash")),
            PolicyDecision::Escalate
        );
        assert_eq!(
            engine.evaluate(&make_request("write", "/repo/.env")),
            PolicyDecision::Deny
        );
    }

    #[test]
    fn missing_policy_file_uses_defaults() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent_policy.toml");
        let engine = PolicyEngine::load(&path).unwrap();

        // Should behave like default policy
        assert_eq!(
            engine.evaluate(&make_request("read", "/repo/worktrees/agent-1/src/main.rs")),
            PolicyDecision::Allow
        );
        assert_eq!(
            engine.evaluate(&make_request("execute", "cargo test")),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn load_from_existing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("policy.toml");
        std::fs::write(
            &path,
            r#"
[deny]
read_in_repo = true
"#,
        )
        .unwrap();
        let engine = PolicyEngine::load(&path).unwrap();
        assert_eq!(
            engine.evaluate(&make_request("read", "/repo/worktrees/agent-1/src/main.rs")),
            PolicyDecision::Deny
        );
    }
}
