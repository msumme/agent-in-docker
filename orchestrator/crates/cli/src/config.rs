use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Application configuration derived from the project directory.
pub struct Config {
    pub project_root: PathBuf,
    pub seed_dir: PathBuf,
    pub agents_dir: PathBuf,
    pub orchestrator_bin: PathBuf,
    pub containerfile: PathBuf,
    pub entrypoint: PathBuf,
    pub orchestrator_port: u16,
    pub mcp_port: u16,
    pub image_name: String,
    pub network_name: String,
    pub orchestrator_pid_file: PathBuf,
}

impl Config {
    /// Discover config by finding the project root (where Containerfile is).
    pub fn discover() -> Result<Self> {
        let exe = std::env::current_exe().context("Cannot determine executable path")?;
        // The binary lives at orchestrator/target/debug/agent or similar.
        // Walk up to find the project root (contains Containerfile).
        let mut dir = exe.parent().context("Cannot determine executable parent directory")?.to_path_buf();
        loop {
            if dir.join("Containerfile").exists() {
                break;
            }
            if !dir.pop() {
                // Fallback: try current directory
                dir = std::env::current_dir()?;
                if !dir.join("Containerfile").exists() {
                    bail!("Cannot find project root (no Containerfile found)");
                }
                break;
            }
        }

        Ok(Self {
            seed_dir: dir.join(".claude-container"),
            agents_dir: dir.join(".claude-agents"),
            orchestrator_bin: dir.join("orchestrator/target/debug/orchestrator"),
            containerfile: dir.join("Containerfile"),
            entrypoint: dir.join("scripts/entrypoint.sh"),
            orchestrator_port: 9800,
            mcp_port: 9801,
            image_name: "agent-in-docker".to_string(),
            network_name: "agent-net".to_string(),
            orchestrator_pid_file: PathBuf::from("/tmp/agent-in-docker-orchestrator.pid"),
            project_root: dir,
        })
    }
}

/// Ensure credentials exist in the seed directory.
pub fn ensure_credentials(cfg: &Config) -> Result<()> {
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
/// ephemeral agents get temp dirs.
pub fn setup_agent_dir(cfg: &Config, name: &str, persistent: bool) -> Result<PathBuf> {
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
        println!("==> Created config for agent '{}'", name);
    }

    // Always refresh credentials from seed
    let creds_dest = agent_dir.join(".credentials.json");
    // Remove old symlink or file
    let _ = std::fs::remove_file(&creds_dest);
    std::fs::copy(
        cfg.seed_dir.join(".credentials.json"),
        &creds_dest,
    )?;

    Ok(agent_dir)
}

fn copy_seed_to_agent_dir(seed: &Path, dest: &Path) -> Result<()> {
    for entry in std::fs::read_dir(seed)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Skip credentials (copied separately) and hidden system files
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn setup_agent_dir_creates_persistent() {
        let tmp = tempfile::tempdir().unwrap();
        let seed = tmp.path().join("seed");
        let agents = tmp.path().join("agents");
        fs::create_dir_all(&seed).unwrap();
        fs::write(seed.join(".credentials.json"), "creds").unwrap();
        fs::write(seed.join(".claude.json"), "config").unwrap();
        fs::create_dir_all(seed.join("backups")).unwrap();

        let cfg = Config {
            seed_dir: seed.clone(),
            agents_dir: agents.clone(),
            project_root: tmp.path().to_path_buf(),
            orchestrator_bin: PathBuf::new(),
            containerfile: PathBuf::new(),
            entrypoint: PathBuf::new(),
            orchestrator_port: 9800,
            mcp_port: 9801,
            image_name: String::new(),
            network_name: String::new(),
            orchestrator_pid_file: PathBuf::new(),
        };

        let dir = setup_agent_dir(&cfg, "coder", true).unwrap();
        assert!(dir.join(".credentials.json").exists());
        assert!(dir.join(".claude.json").exists());
        assert!(dir.join("backups").is_dir());

        // Second call reuses existing dir
        let dir2 = setup_agent_dir(&cfg, "coder", true).unwrap();
        assert_eq!(dir, dir2);
    }

    #[test]
    fn setup_agent_dir_ephemeral_is_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let seed = tmp.path().join("seed");
        let agents = tmp.path().join("agents");
        fs::create_dir_all(&seed).unwrap();
        fs::write(seed.join(".credentials.json"), "creds").unwrap();

        let cfg = Config {
            seed_dir: seed,
            agents_dir: agents.clone(),
            project_root: tmp.path().to_path_buf(),
            orchestrator_bin: PathBuf::new(),
            containerfile: PathBuf::new(),
            entrypoint: PathBuf::new(),
            orchestrator_port: 9800,
            mcp_port: 9801,
            image_name: String::new(),
            network_name: String::new(),
            orchestrator_pid_file: PathBuf::new(),
        };

        let dir = setup_agent_dir(&cfg, "temp-1", false).unwrap();
        assert!(dir.join(".credentials.json").exists());

        // Write something extra
        fs::write(dir.join("extra"), "data").unwrap();

        // Second call cleans up
        let dir2 = setup_agent_dir(&cfg, "temp-1", false).unwrap();
        assert!(!dir2.join("extra").exists());
    }

    #[test]
    fn ensure_credentials_fails_without_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = Config {
            seed_dir: tmp.path().to_path_buf(),
            agents_dir: PathBuf::new(),
            project_root: PathBuf::new(),
            orchestrator_bin: PathBuf::new(),
            containerfile: PathBuf::new(),
            entrypoint: PathBuf::new(),
            orchestrator_port: 9800,
            mcp_port: 9801,
            image_name: String::new(),
            network_name: String::new(),
            orchestrator_pid_file: PathBuf::new(),
        };
        assert!(ensure_credentials(&cfg).is_err());
    }
}
