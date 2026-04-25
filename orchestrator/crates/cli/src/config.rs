use anyhow::{bail, Context, Result};
use std::path::PathBuf;

/// CLI-specific configuration derived from the project directory.
/// For agent setup and credentials, use `to_project_config()` and the
/// functions in `orchestrator_core::project_config`.
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
            agents_dir: dir.join(".agents"),
            orchestrator_bin: dir.join("orchestrator/target/debug/orchestrator"),
            containerfile: dir.join("Containerfile"),
            entrypoint: dir.join("scripts/entrypoint.sh"),
            orchestrator_port: std::env::var("ORCHESTRATOR_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(9800),
            mcp_port: std::env::var("MCP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(9801),
            image_name: std::env::var("AGENT_IMAGE").unwrap_or_else(|_| "agent-in-docker".to_string()),
            network_name: std::env::var("AGENT_NETWORK").unwrap_or_else(|_| "agent-net".to_string()),
            orchestrator_pid_file: PathBuf::from("/tmp/agent-in-docker-orchestrator.pid"),
            project_root: dir,
        })
    }

    /// Convert to shared ProjectConfig for use with core setup functions.
    pub fn to_project_config(&self, dolt_port: Option<u16>) -> orchestrator_core::project_config::ProjectConfig {
        orchestrator_core::project_config::ProjectConfig {
            project_root: self.project_root.clone(),
            seed_dir: self.seed_dir.clone(),
            agents_dir: self.agents_dir.clone(),
            orchestrator_port: self.orchestrator_port,
            mcp_port: self.mcp_port,
            image_name: self.image_name.clone(),
            network_name: self.network_name.clone(),
            dolt_port,
        }
    }
}
