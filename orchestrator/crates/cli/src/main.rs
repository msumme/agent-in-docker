mod config;
mod container;
mod login;
mod services;

use anyhow::Result;
use clap::{Parser, Subcommand};
use orchestrator_core::project_config;
use orchestrator_core::types::StartAgentPayload;

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
        /// Agent role. Determines permissions, memory bucket, and (by default)
        /// which role-prompt file is looked up.
        #[arg(long, default_value = "code-agent")]
        role: String,
        /// Role-prompt spec: a bare name (looked up in project/user/bundled
        /// roles dirs) or a file path. Defaults to the role name.
        #[arg(long)]
        role_prompt: Option<String>,
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
            role_prompt,
            name,
            oneshot,
            build,
        } => {
            let named = name.is_some();
            let agent_name = name.unwrap_or_else(|| {
                format!("agent-{}", std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs())
            });
            let mode = if named && !oneshot { "long-running" } else { "oneshot" };

            let project_path = std::fs::canonicalize(&project_path)
                .map_err(|e| anyhow::anyhow!("Invalid project path '{}': {}", project_path, e))?;

            let pcfg = cfg.to_project_config(None);
            project_config::ensure_credentials(&pcfg)?;

            // Named agents: load prior persisted config so relaunch keeps the
            // agent's identity without the user re-specifying flags. CLI args
            // still win when explicitly passed.
            let prior = if named {
                project_config::load_persisted_config(&pcfg, &agent_name)?
            } else {
                None
            };

            // `--role` has a clap default of "code-agent", so we can't tell
            // "user omitted" from "user wrote code-agent" here. We prefer the
            // persisted role when it exists; explicit override is handled by
            // deleting the persisted file or renaming the agent.
            let role = prior
                .as_ref()
                .map(|p| p.role.clone())
                .unwrap_or(role);
            let role_prompt_spec = role_prompt
                .or_else(|| prior.as_ref().and_then(|p| p.role_prompt_spec.clone()));

            println!("==> Agent: {} (role: {}, {})", agent_name, role, mode);

            let agent_dir = project_config::setup_agent_dir(&pcfg, &agent_name, named)?;
            let role_memory_dir = project_config::setup_role_memory_dir(&pcfg, &role)?;

            // Resolve the role prompt (default to role name if no override).
            let resolved_spec = role_prompt_spec
                .clone()
                .unwrap_or_else(|| role.clone());
            let bundled_roles = cfg.project_root.join("roles");
            let role_prompt_text = match project_config::resolve_role_prompt(
                &resolved_spec,
                &project_path,
                &bundled_roles,
            ) {
                Some(p) => {
                    println!("==> Role prompt: {}", p.display());
                    std::fs::read_to_string(&p)
                        .map_err(|e| anyhow::anyhow!("read role prompt {}: {}", p.display(), e))?
                }
                None => {
                    if role_prompt_spec.is_some() {
                        anyhow::bail!(
                            "Role prompt '{}' not found in project, user-global, or bundled roles dirs",
                            resolved_spec
                        );
                    }
                    eprintln!(
                        "==> Warning: no role prompt file found for role '{}' (looked for {}.md in .agents/roles, ~/.agents/roles, and bundled roles). Agent will start without --append-system-prompt.",
                        resolved_spec, resolved_spec
                    );
                    String::new()
                }
            };

            if named {
                let persisted = project_config::PersistedAgentConfig {
                    role: role.clone(),
                    role_prompt_spec: role_prompt_spec.clone(),
                };
                project_config::save_persisted_config(&pcfg, &agent_name, &persisted)?;
            }

            if build || !container::image_exists(&cfg.image_name)? {
                println!("==> Building container image...");
                container::build_image(&cfg)?;
            }

            container::ensure_network(&cfg.network_name)?;
            services::ensure_orchestrator(&cfg)?;
            let dolt_port = services::ensure_dolt(&project_path)?;

            let payload = StartAgentPayload {
                name: agent_name.clone(),
                project_path: project_path.to_string_lossy().to_string(),
                agent_dir: agent_dir.to_string_lossy().to_string(),
                role_memory_dir: role_memory_dir.to_string_lossy().to_string(),
                role_prompt: role_prompt_text,
                seed_credentials: cfg.seed_dir.join(".credentials.json").to_string_lossy().to_string(),
                role,
                mode: mode.to_string(),
                prompt,
                orchestrator_port: cfg.orchestrator_port,
                mcp_port: cfg.mcp_port,
                dolt_port,
                image_name: cfg.image_name.clone(),
                network_name: cfg.network_name.clone(),
            };

            if mode == "long-running" {
                container::launch_long_running(&payload)?;
            } else {
                container::launch_oneshot(&payload)?;
            }

            if !named {
                let _ = std::fs::remove_dir_all(&agent_dir);
            }

            Ok(())
        }
    }
}
