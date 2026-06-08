use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct Role {
    pub name: String,
    pub capabilities: HashMap<String, bool>,
    #[serde(default)]
    pub message_agents_roles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionResult {
    Allow,
    Deny(String),
    NeedsApproval,
}

pub struct PermissionChecker {
    roles: HashMap<String, Role>,
}

impl PermissionChecker {
    pub fn new() -> Self {
        Self {
            roles: HashMap::new(),
        }
    }

    pub fn load_roles_from_dir(&mut self, dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
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

    pub fn check_gh_pr_create(&self, role_name: &str, _base: &str) -> PermissionResult {
        self.check_capability(role_name, "gh_pr_create")
    }
}

impl crate::mcp::PermissionCheck for PermissionChecker {
    fn check_gh_pr_create(&self, role: &str, base: &str) -> PermissionResult {
        PermissionChecker::check_gh_pr_create(self, role, base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_agent_role() -> Role {
        Role {
            name: "code-agent".into(),
            capabilities: [
                ("user_prompt".into(), true),
            ]
            .into(),
            message_agents_roles: vec![],
        }
    }

    fn pr_agent_role() -> Role {
        Role {
            name: "pr-agent".into(),
            capabilities: [("gh_pr_create".into(), true)].into(),
            message_agents_roles: vec![],
        }
    }

    fn make_checker() -> PermissionChecker {
        let mut checker = PermissionChecker::new();
        checker.add_role(code_agent_role());
        checker.add_role(pr_agent_role());
        checker
    }

    #[test]
    fn check_capability_allows_enabled() {
        let checker = make_checker();
        assert_eq!(
            checker.check_capability("code-agent", "user_prompt"),
            PermissionResult::NeedsApproval
        );
    }

    #[test]
    fn check_capability_denies_disabled() {
        let checker = make_checker();
        assert_eq!(
            checker.check_capability("code-agent", "gh_pr_create"),
            PermissionResult::Deny("Role 'code-agent' does not have 'gh_pr_create' capability".into())
        );
    }

    #[test]
    fn check_capability_denies_unknown_role() {
        let checker = make_checker();
        assert_eq!(
            checker.check_capability("nonexistent", "user_prompt"),
            PermissionResult::Deny("Unknown role: nonexistent".into())
        );
    }

    #[test]
    fn check_gh_pr_create_needs_approval_with_capability() {
        let checker = make_checker();
        assert_eq!(
            checker.check_gh_pr_create("pr-agent", "main"),
            PermissionResult::NeedsApproval
        );
    }

    #[test]
    fn check_gh_pr_create_denies_without_capability() {
        let checker = make_checker();
        assert!(matches!(
            checker.check_gh_pr_create("code-agent", "main"),
            PermissionResult::Deny(_)
        ));
    }

    #[test]
    fn check_gh_pr_create_denies_unknown_role() {
        let checker = make_checker();
        assert!(matches!(
            checker.check_gh_pr_create("nonexistent", "main"),
            PermissionResult::Deny(_)
        ));
    }

    #[test]
    fn role_construction_without_removed_fields() {
        let role = Role {
            name: "test".into(),
            capabilities: [("user_prompt".into(), true)].into(),
            message_agents_roles: vec!["review-agent".into()],
        };
        let mut checker = PermissionChecker::new();
        checker.add_role(role);
        assert_eq!(
            checker.check_capability("test", "user_prompt"),
            PermissionResult::NeedsApproval
        );
    }
}
