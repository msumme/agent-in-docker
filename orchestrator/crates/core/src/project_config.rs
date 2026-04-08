use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
            agents_dir: root.join(".claude-agents"),
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
