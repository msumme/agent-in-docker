use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Config persisted for a named agent so re-launches inherit its setup.
/// Lives at `<agents_dir>/<name>.json` (sibling to the agent_dir, so it is
/// not mounted into the container).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedAgentConfig {
    pub role: String,
    /// What the user originally passed for `--role-prompt` (name or path).
    /// Re-resolved on each launch so edits to bundled/global files take effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_prompt_spec: Option<String>,
}

/// Project configuration -- shared between CLI and orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub project_root: PathBuf,
    pub seed_dir: PathBuf,
    pub agents_dir: PathBuf,
    pub orchestrator_port: u16,
    pub mcp_port: u16,
    pub image_name: String,
    pub network_name: String,
    pub dolt_port: Option<u16>,
}

impl ProjectConfig {
    /// Build from a known project root.
    pub fn from_root(root: PathBuf) -> Self {
        Self {
            seed_dir: root.join(".claude-container"),
            agents_dir: root.join(".agents"),
            orchestrator_port: std::env::var("ORCHESTRATOR_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(9800),
            mcp_port: std::env::var("MCP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(9801),
            image_name: std::env::var("AGENT_IMAGE").unwrap_or_else(|_| "agent-in-docker".to_string()),
            network_name: std::env::var("AGENT_NETWORK").unwrap_or_else(|_| "agent-net".to_string()),
            dolt_port: None,
            project_root: root,
        }
    }
}

/// Ensure credentials exist in the seed directory.
pub fn ensure_credentials(cfg: &ProjectConfig) -> Result<()> {
    let creds = cfg.seed_dir.join(".credentials.json");
    if !creds.exists() {
        bail!(
            "No credentials found in {}\nRun: agent login",
            cfg.seed_dir.display()
        );
    }
    Ok(())
}

/// Set up a per-agent config directory. Named agents get persistent dirs,
/// ephemeral agents get fresh dirs.
pub fn setup_agent_dir(cfg: &ProjectConfig, name: &str, persistent: bool) -> Result<PathBuf> {
    std::fs::create_dir_all(&cfg.agents_dir)?;

    let agent_dir = if persistent {
        cfg.agents_dir.join(name)
    } else {
        let dir = cfg.agents_dir.join(format!("ephemeral-{}", name));
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        dir
    };

    if !agent_dir.exists() {
        std::fs::create_dir_all(&agent_dir)?;
        copy_seed_to_agent_dir(&cfg.seed_dir, &agent_dir)?;
    }

    // Always refresh credentials from seed
    let creds_dest = agent_dir.join(".credentials.json");
    let _ = std::fs::remove_file(&creds_dest);
    std::fs::copy(cfg.seed_dir.join(".credentials.json"), &creds_dest)?;

    Ok(agent_dir)
}

fn copy_seed_to_agent_dir(seed: &Path, dest: &Path) -> Result<()> {
    for entry in std::fs::read_dir(seed)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == ".credentials.json" {
            continue;
        }
        let src = entry.path();
        let dst = dest.join(&name);
        if src.is_dir() {
            copy_dir_recursive(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let s = entry.path();
        let d = dst.join(entry.file_name());
        if s.is_dir() {
            copy_dir_recursive(&s, &d)?;
        } else {
            std::fs::copy(&s, &d)?;
        }
    }
    Ok(())
}

/// Set up a role-scoped memory directory. All agents sharing a role share this
/// Claude Code `projects/` tree, so memory accumulated by one agent under that
/// role is available to future agents of the same role — while remaining
/// isolated from other roles' memory.
pub fn setup_role_memory_dir(cfg: &ProjectConfig, role: &str) -> Result<PathBuf> {
    let dir = cfg
        .agents_dir
        .join("_role-memory")
        .join(role)
        .join("projects");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Path where a named agent's persisted config lives. Sibling to agent_dir
/// so it isn't mounted into the container.
pub fn persisted_config_path(cfg: &ProjectConfig, name: &str) -> PathBuf {
    cfg.agents_dir.join(format!("{}.json", name))
}

/// Load persisted config for a named agent, if any.
pub fn load_persisted_config(cfg: &ProjectConfig, name: &str) -> Result<Option<PersistedAgentConfig>> {
    let path = persisted_config_path(cfg, name);
    if !path.exists() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("read persisted agent config {}", path.display()))?;
    let parsed: PersistedAgentConfig = serde_json::from_str(&contents)
        .with_context(|| format!("parse persisted agent config {}", path.display()))?;
    Ok(Some(parsed))
}

/// Save persisted config for a named agent.
pub fn save_persisted_config(
    cfg: &ProjectConfig,
    name: &str,
    persisted: &PersistedAgentConfig,
) -> Result<()> {
    std::fs::create_dir_all(&cfg.agents_dir)?;
    let path = persisted_config_path(cfg, name);
    let json = serde_json::to_string_pretty(persisted)?;
    std::fs::write(&path, json)
        .with_context(|| format!("write persisted agent config {}", path.display()))?;
    Ok(())
}

/// Resolve a role-prompt spec to an absolute path.
///
/// `spec` may be:
///   - absolute path
///   - relative path (starts with `./` or `../`, or contains `/`, or ends with `.md`)
///   - bare name — looked up through three tiers, first match wins:
///     1. `<target_project>/.agents/roles/<name>.md`
///     2. `~/.agents/roles/<name>.md`
///     3. `<bundled_roles_dir>/<name>.md`
///
/// Returns `None` if no file is found. `bundled_roles_dir` is the `roles/`
/// directory shipped with agent-in-docker itself.
pub fn resolve_role_prompt(
    spec: &str,
    target_project: &Path,
    bundled_roles_dir: &Path,
) -> Option<PathBuf> {
    if looks_like_path(spec) {
        let p = if Path::new(spec).is_absolute() {
            PathBuf::from(spec)
        } else {
            target_project.join(spec)
        };
        return if p.is_file() { Some(p) } else { None };
    }

    let filename = format!("{}.md", spec);

    let project_local = target_project.join(".agents/roles").join(&filename);
    if project_local.is_file() {
        return Some(project_local);
    }

    if let Some(home) = home_dir() {
        let user_global = home.join(".agents/roles").join(&filename);
        if user_global.is_file() {
            return Some(user_global);
        }
    }

    let bundled = bundled_roles_dir.join(&filename);
    if bundled.is_file() {
        return Some(bundled);
    }

    None
}

fn looks_like_path(spec: &str) -> bool {
    Path::new(spec).is_absolute()
        || spec.starts_with("./")
        || spec.starts_with("../")
        || spec.contains('/')
        || spec.ends_with(".md")
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Find the latest .claude.json backup file in a directory.
pub fn find_latest_backup(dir: &Path) -> Result<Option<PathBuf>> {
    if !dir.exists() {
        return Ok(None);
    }
    let mut backups: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".claude.json.backup.")
        })
        .map(|e| e.path())
        .collect();
    backups.sort();
    Ok(backups.last().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn setup_agent_dir_creates_persistent() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = ProjectConfig {
            project_root: tmp.path().to_path_buf(),
            seed_dir: tmp.path().join("seed"),
            agents_dir: tmp.path().join("agents"),
            orchestrator_port: 9800,
            mcp_port: 9801,
            image_name: String::new(),
            network_name: String::new(),
            dolt_port: None,
        };
        fs::create_dir_all(&cfg.seed_dir).unwrap();
        fs::write(cfg.seed_dir.join(".credentials.json"), "creds").unwrap();
        fs::write(cfg.seed_dir.join(".claude.json"), "config").unwrap();

        let dir = setup_agent_dir(&cfg, "coder", true).unwrap();
        assert!(dir.join(".credentials.json").exists());
        assert!(dir.join(".claude.json").exists());
    }

    #[test]
    fn setup_agent_dir_ephemeral_is_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = ProjectConfig {
            project_root: tmp.path().to_path_buf(),
            seed_dir: tmp.path().join("seed"),
            agents_dir: tmp.path().join("agents"),
            orchestrator_port: 9800,
            mcp_port: 9801,
            image_name: String::new(),
            network_name: String::new(),
            dolt_port: None,
        };
        fs::create_dir_all(&cfg.seed_dir).unwrap();
        fs::write(cfg.seed_dir.join(".credentials.json"), "creds").unwrap();

        let dir = setup_agent_dir(&cfg, "temp", false).unwrap();
        fs::write(dir.join("extra"), "data").unwrap();

        let dir2 = setup_agent_dir(&cfg, "temp", false).unwrap();
        assert!(!dir2.join("extra").exists());
    }

    #[test]
    fn find_latest_backup_returns_newest() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".claude.json.backup.100"), "old").unwrap();
        fs::write(dir.path().join(".claude.json.backup.200"), "new").unwrap();
        fs::write(dir.path().join("other-file"), "ignore").unwrap();

        let result = find_latest_backup(dir.path()).unwrap();
        assert!(result.is_some());
        assert!(result.unwrap().to_str().unwrap().contains("200"));
    }

    #[test]
    fn find_latest_backup_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_latest_backup(dir.path()).unwrap().is_none());
    }

    #[test]
    fn find_latest_backup_missing_dir() {
        assert!(find_latest_backup(Path::new("/nonexistent")).unwrap().is_none());
    }

    fn test_cfg(root: &Path) -> ProjectConfig {
        ProjectConfig {
            project_root: root.to_path_buf(),
            seed_dir: root.join("seed"),
            agents_dir: root.join("agents"),
            orchestrator_port: 9800,
            mcp_port: 9801,
            image_name: String::new(),
            network_name: String::new(),
            dolt_port: None,
        }
    }

    #[test]
    fn resolve_role_prompt_prefers_project_local() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let bundled = tmp.path().join("bundled");
        fs::create_dir_all(project.join(".agents/roles")).unwrap();
        fs::create_dir_all(&bundled).unwrap();
        fs::write(project.join(".agents/roles/architect.md"), "project").unwrap();
        fs::write(bundled.join("architect.md"), "bundled").unwrap();

        let resolved = resolve_role_prompt("architect", &project, &bundled).unwrap();
        assert_eq!(fs::read_to_string(resolved).unwrap(), "project");
    }

    #[test]
    fn resolve_role_prompt_falls_back_to_bundled() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let bundled = tmp.path().join("bundled");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&bundled).unwrap();
        fs::write(bundled.join("cleaner.md"), "bundled").unwrap();

        let resolved = resolve_role_prompt("cleaner", &project, &bundled).unwrap();
        assert_eq!(fs::read_to_string(resolved).unwrap(), "bundled");
    }

    #[test]
    fn resolve_role_prompt_absolute_path() {
        let tmp = tempfile::tempdir().unwrap();
        let prompt = tmp.path().join("custom.md");
        fs::write(&prompt, "custom").unwrap();

        let resolved =
            resolve_role_prompt(prompt.to_str().unwrap(), tmp.path(), tmp.path()).unwrap();
        assert_eq!(resolved, prompt);
    }

    #[test]
    fn resolve_role_prompt_missing_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(resolve_role_prompt("nope", tmp.path(), tmp.path()).is_none());
    }

    #[test]
    fn persisted_config_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_cfg(tmp.path());
        let original = PersistedAgentConfig {
            role: "architect".into(),
            role_prompt_spec: Some("/abs/path.md".into()),
        };
        save_persisted_config(&cfg, "alice", &original).unwrap();
        let loaded = load_persisted_config(&cfg, "alice").unwrap().unwrap();
        assert_eq!(loaded.role, "architect");
        assert_eq!(loaded.role_prompt_spec.as_deref(), Some("/abs/path.md"));
    }

    #[test]
    fn load_persisted_config_missing_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_cfg(tmp.path());
        assert!(load_persisted_config(&cfg, "missing").unwrap().is_none());
    }

    #[test]
    fn ensure_credentials_fails_without_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = ProjectConfig {
            project_root: tmp.path().to_path_buf(),
            seed_dir: tmp.path().to_path_buf(),
            agents_dir: PathBuf::new(),
            orchestrator_port: 9800,
            mcp_port: 9801,
            image_name: String::new(),
            network_name: String::new(),
            dolt_port: None,
        };
        assert!(ensure_credentials(&cfg).is_err());
    }
}
