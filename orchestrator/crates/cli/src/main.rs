mod auth;
mod config;
mod container;
mod image_resolver;
mod login;
mod services;
mod team_cmd;

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
        /// which role-prompt file is looked up. Defaults to maintenance-producer
        /// — the safer producer that addresses existing tickets rather than
        /// inventing new features. Pick `feature-producer` for new capability,
        /// or one of the reviewers (`architect`, `cleaner`, `review-agent`).
        #[arg(long, default_value = "maintenance-producer")]
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
    /// Manage agent teams (PR-scoped groups of planner/producer/reviewer)
    Team {
        #[command(subcommand)]
        action: TeamAction,
    },
}

#[derive(Subcommand)]
enum TeamAction {
    /// Spawn a team for a bd ticket (provisions worktree + 3 agents)
    Spawn {
        /// bd ticket id (e.g. agent-in-docker-0fw.2)
        ticket_id: String,
        /// Base branch the team's worktree branches from
        #[arg(long, default_value = "main")]
        base: String,
        /// Use maintenance-producer instead of feature-producer
        #[arg(long)]
        maintenance: bool,
    },
    /// Suspend a team (kill containers; state preserved on disk)
    Suspend {
        team_id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Resume a suspended team (restart containers; primer injected)
    Resume {
        team_id: String,
        /// Resume only the named role (planner | feature-producer |
        /// maintenance-producer | review-agent). Default: all roles.
        #[arg(long)]
        role: Option<String>,
    },
    /// List all known teams (active and suspended)
    List,
    /// Show one team's manifest details
    Status { team_id: String },
    /// Force-teardown a team (containers gone, worktree removed, manifest archived/deleted)
    Kill {
        team_id: String,
        /// Skip archive — delete the team dir entirely
        #[arg(long)]
        no_archive: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config::Config::discover()?;

    match cli.command {
        Commands::Login => login::run_login(&cfg),
        Commands::Team { action } => match action {
            TeamAction::Spawn {
                ticket_id,
                base,
                maintenance,
            } => team_cmd::cmd_spawn(&cfg, &ticket_id, &base, maintenance),
            TeamAction::Suspend { team_id, reason } => {
                team_cmd::cmd_suspend(&cfg, &team_id, reason)
            }
            TeamAction::Resume { team_id, role } => team_cmd::cmd_resume(&cfg, &team_id, role),
            TeamAction::List => team_cmd::cmd_list(&cfg),
            TeamAction::Status { team_id } => team_cmd::cmd_status(&cfg, &team_id),
            TeamAction::Kill { team_id, no_archive } => {
                team_cmd::cmd_kill(&cfg, &team_id, !no_archive)
            }
        },
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

            // Coordination layer is bd. Refuse to launch against a project that
            // hasn't been initialized — agents would have nowhere to file
            // tickets, claim work, or coordinate edits.
            let bd_marker = project_path.join(".beads").join("config.yaml");
            if !bd_marker.is_file() {
                anyhow::bail!(
                    "Project '{}' is not bd-enabled (no .beads/config.yaml found). \
                     Initialize it with `bd init` from the project root, then retry.",
                    project_path.display()
                );
            }

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

            // Prepend the shared meta-prompt (coordination + coding standards)
            // so every role inherits the same baseline. Resolved through the
            // same 3-tier search as roles, under the bare name "_meta".
            let meta_prompt_text = project_config::resolve_role_prompt(
                "_meta",
                &project_path,
                &bundled_roles,
            )
            .and_then(|p| {
                println!("==> Meta prompt: {}", p.display());
                std::fs::read_to_string(&p).ok()
            })
            .unwrap_or_default();

            let role_prompt_text = if meta_prompt_text.is_empty() {
                role_prompt_text
            } else if role_prompt_text.is_empty() {
                meta_prompt_text
            } else {
                format!("{}\n\n---\n\n{}", meta_prompt_text, role_prompt_text)
            };

            if named {
                let persisted = project_config::PersistedAgentConfig {
                    role: role.clone(),
                    role_prompt_spec: role_prompt_spec.clone(),
                };
                project_config::save_persisted_config(&pcfg, &agent_name, &persisted)?;
            }

            let resolved = image_resolver::resolve_for(&project_path, &role);
            if build {
                image_resolver::ensure_base_image(&cfg)?;
                // Forced rebuild: blow the per-role tag away so ensure_image
                // re-runs `podman build` instead of seeing the cached tag.
                let _ = std::process::Command::new("podman")
                    .args(["image", "rm", "-f", &resolved.image_name])
                    .status();
            }
            image_resolver::ensure_image(&cfg, &resolved)?;

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
                image_name: resolved.image_name,
                network_name: cfg.network_name.clone(),
                extra_mounts: vec![],
                model: None,
                effort: None,
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
