use anyhow::Result;
use orchestrator_core::project_config::discover_home_root;
use std::path::PathBuf;

/// CLI-specific configuration. Two roots: `home_root` is agent-in-docker itself
/// (bundled `roles/`, `Containerfile.base`, the orchestrator binary, seed
/// credentials); `project_root` is the target repo a team operates on — the
/// current directory's git root. They coincide only when run from within
/// agent-in-docker. For agent setup and credentials, use `to_project_config()`
/// and the functions in `orchestrator_core::project_config`.
pub struct Config {
    pub home_root: PathBuf,
    pub project_root: PathBuf,
    pub seed_dir: PathBuf,
    pub agents_dir: PathBuf,
    pub orchestrator_bin: PathBuf,
    pub containerfile: PathBuf,
    pub orchestrator_port: u16,
    pub mcp_port: u16,
    pub image_name: String,
    pub network_name: String,
    pub orchestrator_pid_file: PathBuf,
}

/// The git root of the current directory, or `None` if not inside a work tree.
fn git_root_of_cwd() -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8(out.stdout).ok()?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

impl Config {
    /// Bundled assets resolve against the agent-in-docker home repo (discovered
    /// from the executable); the target project is the current directory's git
    /// root, falling back to home when not run from inside another repo.
    pub fn discover() -> Result<Self> {
        let home = discover_home_root()?;
        let project_root = git_root_of_cwd().unwrap_or_else(|| home.clone());

        Ok(Self {
            seed_dir: home.join(".claude-container"),
            agents_dir: home.join(".agents"),
            orchestrator_bin: home.join("orchestrator/target/debug/orchestrator"),
            containerfile: home.join("Containerfile.base"),
            orchestrator_port: std::env::var("ORCHESTRATOR_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(9800),
            mcp_port: std::env::var("MCP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(9801),
            image_name: std::env::var("AGENT_IMAGE").unwrap_or_else(|_| "agent-in-docker".to_string()),
            network_name: std::env::var("AGENT_NETWORK").unwrap_or_else(|_| "agent-net".to_string()),
            orchestrator_pid_file: PathBuf::from("/tmp/agent-in-docker-orchestrator.pid"),
            home_root: home,
            project_root,
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
