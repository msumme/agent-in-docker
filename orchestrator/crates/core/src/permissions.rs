use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Role {
    pub name: String,
    pub capabilities: HashMap<String, bool>,
    #[serde(default)]
    pub file_read_paths: Vec<String>,
    #[serde(default)]
    pub file_read_deny_paths: Vec<String>,
    #[serde(default)]
    pub git_push_remotes: Vec<String>,
    #[serde(default)]
    pub message_agents_roles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionResult {
    Allow,
    Deny(String),
    NeedsApproval,
}

/// Hardcoded paths that are always denied regardless of role config.
const HARDCODED_DENY_PATTERNS: &[&str] = &[
    "**/.ssh/id_*",
    "**/.aws/credentials",
    "**/.config/gcloud/application_default_credentials.json",
    "**/.claude/.credentials.json",
];

pub trait EnvResolver: Send + Sync {
    fn home_dir(&self) -> Option<PathBuf>;
}

pub struct RealEnvResolver;

impl EnvResolver for RealEnvResolver {
    fn home_dir(&self) -> Option<PathBuf> {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

pub struct PermissionChecker {
    roles: HashMap<String, Role>,
    env: Box<dyn EnvResolver>,
}

impl PermissionChecker {
    pub fn new(env: Box<dyn EnvResolver>) -> Self {
        Self {
            roles: HashMap::new(),
            env,
        }
    }

    pub fn load_roles_from_dir(&mut self, dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
        if !dir.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "yml" || e == "yaml") {
                let content = std::fs::read_to_string(&path)?;
                let role: Role = serde_yaml::from_str(&content)?;
                self.roles.insert(role.name.clone(), role);
            }
        }
        Ok(())
    }

    pub fn add_role(&mut self, role: Role) {
        self.roles.insert(role.name.clone(), role);
    }

    pub fn get_role(&self, name: &str) -> Option<&Role> {
        self.roles.get(name)
    }

    pub fn check_capability(&self, role_name: &str, capability: &str) -> PermissionResult {
        match self.roles.get(role_name) {
            None => PermissionResult::Deny(format!("Unknown role: {}", role_name)),
            Some(role) => match role.capabilities.get(capability) {
                Some(true) => PermissionResult::NeedsApproval,
                _ => PermissionResult::Deny(format!(
                    "Role '{}' does not have '{}' capability",
                    role_name, capability
                )),
            },
        }
    }

    pub fn check_file_read(&self, role_name: &str, path: &str) -> PermissionResult {
        // First check capability
        if let PermissionResult::Deny(reason) = self.check_capability(role_name, "file_read") {
            return PermissionResult::Deny(reason);
        }

        let resolved = self.resolve_path(path);

        // Hardcoded denials -- always reject
        for pattern in HARDCODED_DENY_PATTERNS {
            let expanded = self.expand_env(pattern);
            if self.path_matches(&resolved, &expanded) {
                return PermissionResult::Deny(format!(
                    "Path '{}' is blocked by security policy",
                    path
                ));
            }
        }

        let role = self.roles.get(role_name).unwrap();

        // Role deny patterns -- checked before allow
        for pattern in &role.file_read_deny_paths {
            let expanded = self.expand_env(pattern);
            if self.path_matches(&resolved, &expanded) {
                return PermissionResult::Deny(format!(
                    "Path '{}' matches deny pattern '{}'",
                    path, pattern
                ));
            }
        }

        // Role allow patterns
        for pattern in &role.file_read_paths {
            let expanded = self.expand_env(pattern);
            if self.path_matches(&resolved, &expanded) {
                return PermissionResult::NeedsApproval;
            }
        }

        PermissionResult::Deny(format!(
            "Path '{}' is not in the allowed list for role '{}'",
            path, role_name
        ))
    }

    pub fn check_git_push(&self, role_name: &str, remote: &str) -> PermissionResult {
        if let PermissionResult::Deny(reason) = self.check_capability(role_name, "git_push") {
            return PermissionResult::Deny(reason);
        }

        let role = self.roles.get(role_name).unwrap();
        if role.git_push_remotes.iter().any(|r| r == "*" || r == remote) {
            PermissionResult::NeedsApproval
        } else {
            PermissionResult::Deny(format!(
                "Remote '{}' is not allowed for role '{}'",
                remote, role_name
            ))
        }
    }

    fn resolve_path(&self, path: &str) -> String {
        let expanded = self.expand_env(path);
        // Resolve .. components to prevent traversal
        let path_buf = PathBuf::from(&expanded);
        match std::fs::canonicalize(&path_buf) {
            Ok(canonical) => canonical.to_string_lossy().to_string(),
            Err(_) => {
                // If file doesn't exist, do basic normalization
                let mut components = Vec::new();
                for component in path_buf.components() {
                    match component {
                        std::path::Component::ParentDir => {
                            components.pop();
                        }
                        std::path::Component::CurDir => {}
                        other => components.push(other),
                    }
                }
                let normalized: PathBuf = components.iter().collect();
                normalized.to_string_lossy().to_string()
            }
        }
    }

    fn expand_env(&self, s: &str) -> String {
        let mut result = s.to_string();
        if let Some(home) = self.env.home_dir() {
            result = result.replace("${HOME}", &home.to_string_lossy());
            // Also handle ~ at start of path
            if result.starts_with("~/") {
                result = format!("{}/{}", home.to_string_lossy(), &result[2..]);
            }
        }
        result
    }

    fn path_matches(&self, path: &str, pattern: &str) -> bool {
        // Simple glob matching: ** matches any path segments, * matches within a segment
        let glob_pattern = glob::Pattern::new(pattern);
        match glob_pattern {
            Ok(p) => p.matches(path),
            Err(_) => path == pattern, // Fall back to exact match
        }
    }
}

impl crate::mcp::PermissionCheck for PermissionChecker {
    fn check_file_read(&self, role: &str, path: &str) -> PermissionResult {
        PermissionChecker::check_file_read(self, role, path)
    }
    fn check_git_push(&self, role: &str, remote: &str) -> PermissionResult {
        PermissionChecker::check_git_push(self, role, remote)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeEnv {
        home: PathBuf,
    }

    impl FakeEnv {
        fn new(home: &str) -> Self {
            Self {
                home: PathBuf::from(home),
            }
        }
    }

    impl EnvResolver for FakeEnv {
        fn home_dir(&self) -> Option<PathBuf> {
            Some(self.home.clone())
        }
    }

    fn code_agent_role() -> Role {
        Role {
            name: "code-agent".into(),
            capabilities: [
                ("file_read".into(), true),
                ("git_push".into(), true),
                ("user_prompt".into(), true),
            ]
            .into(),
            file_read_paths: vec![
                "${HOME}/.gitconfig".into(),
                "${HOME}/.ssh/config".into(),
            ],
            file_read_deny_paths: vec![
                "**/*.pem".into(),
                "**/*.key".into(),
            ],
            git_push_remotes: vec!["origin".into()],
            message_agents_roles: vec![],
        }
    }

    fn review_role() -> Role {
        Role {
            name: "review-agent".into(),
            capabilities: [
                ("file_read".into(), true),
                ("git_push".into(), false),
                ("user_prompt".into(), true),
            ]
            .into(),
            file_read_paths: vec!["${HOME}/.gitconfig".into()],
            file_read_deny_paths: vec![],
            git_push_remotes: vec![],
            message_agents_roles: vec![],
        }
    }

    fn make_checker() -> PermissionChecker {
        let mut checker = PermissionChecker::new(Box::new(FakeEnv::new("/home/testuser")));
        checker.add_role(code_agent_role());
        checker.add_role(review_role());
        checker
    }

    #[test]
    fn check_capability_allows_enabled() {
        let checker = make_checker();
        assert_eq!(
            checker.check_capability("code-agent", "file_read"),
            PermissionResult::NeedsApproval
        );
    }

    #[test]
    fn check_capability_denies_disabled() {
        let checker = make_checker();
        assert_eq!(
            checker.check_capability("review-agent", "git_push"),
            PermissionResult::Deny("Role 'review-agent' does not have 'git_push' capability".into())
        );
    }

    #[test]
    fn check_capability_denies_unknown_role() {
        let checker = make_checker();
        assert_eq!(
            checker.check_capability("nonexistent", "file_read"),
            PermissionResult::Deny("Unknown role: nonexistent".into())
        );
    }

    #[test]
    fn file_read_allows_matching_path() {
        let checker = make_checker();
        assert_eq!(
            checker.check_file_read("code-agent", "/home/testuser/.gitconfig"),
            PermissionResult::NeedsApproval
        );
    }

    #[test]
    fn file_read_denies_unmatched_path() {
        let checker = make_checker();
        assert!(matches!(
            checker.check_file_read("code-agent", "/etc/passwd"),
            PermissionResult::Deny(_)
        ));
    }

    #[test]
    fn file_read_denies_role_deny_pattern() {
        let checker = make_checker();
        assert!(matches!(
            checker.check_file_read("code-agent", "/some/path/cert.pem"),
            PermissionResult::Deny(_)
        ));
    }

    #[test]
    fn file_read_denies_hardcoded_ssh_keys() {
        let checker = make_checker();
        let result = checker.check_file_read("code-agent", "/home/testuser/.ssh/id_rsa");
        assert!(matches!(result, PermissionResult::Deny(_)));
        if let PermissionResult::Deny(msg) = result {
            assert!(msg.contains("security policy"));
        }
    }

    #[test]
    fn file_read_denies_hardcoded_aws_creds() {
        let checker = make_checker();
        assert!(matches!(
            checker.check_file_read("code-agent", "/home/testuser/.aws/credentials"),
            PermissionResult::Deny(_)
        ));
    }

    #[test]
    fn file_read_denies_if_capability_disabled() {
        let mut checker = PermissionChecker::new(Box::new(FakeEnv::new("/home/test")));
        checker.add_role(Role {
            name: "no-read".into(),
            capabilities: [("file_read".into(), false)].into(),
            file_read_paths: vec!["**/*".into()],
            file_read_deny_paths: vec![],
            git_push_remotes: vec![],
            message_agents_roles: vec![],
        });
        assert!(matches!(
            checker.check_file_read("no-read", "/anything"),
            PermissionResult::Deny(_)
        ));
    }

    #[test]
    fn git_push_allows_matching_remote() {
        let checker = make_checker();
        assert_eq!(
            checker.check_git_push("code-agent", "origin"),
            PermissionResult::NeedsApproval
        );
    }

    #[test]
    fn git_push_denies_unmatched_remote() {
        let checker = make_checker();
        assert!(matches!(
            checker.check_git_push("code-agent", "upstream"),
            PermissionResult::Deny(_)
        ));
    }

    #[test]
    fn git_push_denies_when_capability_disabled() {
        let checker = make_checker();
        assert!(matches!(
            checker.check_git_push("review-agent", "origin"),
            PermissionResult::Deny(_)
        ));
    }

    #[test]
    fn expand_env_replaces_home() {
        let checker = make_checker();
        assert_eq!(
            checker.expand_env("${HOME}/.gitconfig"),
            "/home/testuser/.gitconfig"
        );
    }

    #[test]
    fn expand_env_replaces_tilde() {
        let checker = make_checker();
        assert_eq!(
            checker.expand_env("~/.gitconfig"),
            "/home/testuser/.gitconfig"
        );
    }
}
