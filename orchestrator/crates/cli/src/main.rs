mod config;
mod container;
mod login;
mod services;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "agent", about = "Run LLM agents in containers")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch an agent in a container
    Run {
        /// Project directory to mount
        project_path: String,
        /// Prompt or task for the agent
        prompt: String,
        /// Agent role
        #[arg(long, default_value = "code-agent")]
        role: String,
        /// Agent name (makes it persistent and long-running)
        #[arg(long)]
        name: Option<String>,
        /// Run as one-shot even if named
        #[arg(long)]
        oneshot: bool,
        /// Force rebuild container image
        #[arg(long)]
        build: bool,
    },
    /// Authenticate with Claude (opens browser)
    Login,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config::Config::discover()?;

    match cli.command {
        Commands::Login => login::run_login(&cfg),
        Commands::Run {
            project_path,
            prompt,
            role,
            name,
            oneshot,
            build,
        } => {
            let named = name.is_some();
            let agent_name = name.unwrap_or_else(|| {
                format!(
                    "agent-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                )
            });
            let mode = if named && !oneshot {
                "long-running"
            } else {
                "oneshot"
            };

            let project_path = std::fs::canonicalize(&project_path)
                .map_err(|e| anyhow::anyhow!("Invalid project path '{}': {}", project_path, e))?;

            println!("==> Agent: {} (role: {}, {})", agent_name, role, mode);

            // Ensure credentials exist
            config::ensure_credentials(&cfg)?;

            // Set up per-agent config directory
            let agent_dir =
                config::setup_agent_dir(&cfg, &agent_name, named)?;

            // Ensure container image exists
            if build || !container::image_exists(&cfg.image_name)? {
                println!("==> Building container image...");
                container::build_image(&cfg)?;
            }

            // Ensure network exists
            container::ensure_network(&cfg.network_name)?;

            // Ensure orchestrator is running
            services::ensure_orchestrator(&cfg)?;

            // Ensure bridge is running
            services::ensure_bridge(&cfg)?;

            // Launch agent
            let run_cfg = container::RunConfig {
                agent_name: agent_name.clone(),
                project_path: project_path.to_string_lossy().to_string(),
                agent_dir: agent_dir.to_string_lossy().to_string(),
                role,
                mode: mode.to_string(),
                prompt,
                orchestrator_port: cfg.orchestrator_port,
                bridge_port: cfg.bridge_port,
                image_name: cfg.image_name.clone(),
                network_name: cfg.network_name.clone(),
            };

            if mode == "long-running" {
                container::launch_long_running(&run_cfg)?;
            } else {
                container::launch_oneshot(&run_cfg)?;
            }

            // Clean up ephemeral agent dir
            if !named {
                let _ = std::fs::remove_dir_all(&agent_dir);
            }

            Ok(())
        }
    }
}
