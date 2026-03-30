mod config;
mod container;
mod login;
mod services;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;

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

#[tokio::main]
async fn main() -> Result<()> {
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
                format!("agent-{}", std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs())
            });
            let mode = if named && !oneshot { "long-running" } else { "oneshot" };

            let project_path = std::fs::canonicalize(&project_path)
                .map_err(|e| anyhow::anyhow!("Invalid project path '{}': {}", project_path, e))?;

            println!("==> Agent: {} (role: {}, {})", agent_name, role, mode);

            // Ensure credentials exist
            config::ensure_credentials(&cfg)?;

            // Set up per-agent config directory
            let agent_dir = config::setup_agent_dir(&cfg, &agent_name, named)?;

            // Ensure container image exists
            if build || !container::image_exists(&cfg.image_name)? {
                println!("==> Building container image...");
                container::build_image(&cfg)?;
            }

            // Ensure network exists
            container::ensure_network(&cfg.network_name)?;

            // Ensure orchestrator is running
            services::ensure_orchestrator(&cfg)?;

            // Ensure dolt is running for beads
            let dolt_port = services::ensure_dolt(&project_path)?;

            // Send start_agent to orchestrator via WS
            let payload = serde_json::json!({
                "name": agent_name,
                "role": role,
                "mode": mode,
                "project_path": project_path.to_string_lossy(),
                "prompt": prompt,
                "agent_dir": agent_dir.to_string_lossy(),
                "image_name": cfg.image_name,
                "network_name": cfg.network_name,
                "orchestrator_port": cfg.orchestrator_port,
                "mcp_port": cfg.mcp_port,
                "dolt_port": dolt_port,
            });

            let ws_url = format!("ws://localhost:{}", cfg.orchestrator_port);
            match send_ws_command(&ws_url, "start_agent", payload).await {
                Ok(response) => {
                    let success = response.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
                    if success {
                        println!("==> Agent '{}' started by orchestrator", agent_name);
                        println!("    Attach to agent:       tmux attach -t orchestrator");
                        println!("    Orchestrator TUI:      tmux attach -t orchestrator");
                    } else {
                        let msg = response.get("message").and_then(|v| v.as_str()).unwrap_or("Unknown error");
                        // Fallback: start directly (orchestrator may not have AgentManager)
                        eprintln!("==> Orchestrator couldn't start agent ({}), launching directly...", msg);
                        let run_cfg = container::RunConfig {
                            agent_name: agent_name.clone(),
                            project_path: project_path.to_string_lossy().to_string(),
                            agent_dir: agent_dir.to_string_lossy().to_string(),
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
                            container::launch_long_running(&run_cfg)?;
                        } else {
                            container::launch_oneshot(&run_cfg)?;
                        }
                    }
                }
                Err(e) => {
                    // WS connection failed -- orchestrator might be old version, fall back
                    eprintln!("==> WS command failed ({}), launching directly...", e);
                    let run_cfg = container::RunConfig {
                        agent_name: agent_name.clone(),
                        project_path: project_path.to_string_lossy().to_string(),
                        agent_dir: agent_dir.to_string_lossy().to_string(),
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
                        container::launch_long_running(&run_cfg)?;
                    } else {
                        container::launch_oneshot(&run_cfg)?;
                    }
                }
            }

            // Clean up ephemeral agent dir
            if !named {
                let _ = std::fs::remove_dir_all(&agent_dir);
            }

            Ok(())
        }
    }
}

/// Send a command to the orchestrator via WebSocket and wait for the ack.
async fn send_ws_command(url: &str, msg_type: &str, payload: serde_json::Value) -> Result<serde_json::Value> {
    let (ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .context("Failed to connect to orchestrator WS")?;

    let (mut sender, mut receiver) = ws.split();

    let msg = serde_json::json!({
        "id": format!("cli-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()),
        "type": msg_type,
        "from": "cli",
        "payload": payload,
    });

    sender.send(WsMessage::Text(serde_json::to_string(&msg)?.into())).await?;

    // Wait for ack (5s timeout)
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(Ok(WsMessage::Text(text))) = receiver.next().await {
            let resp: serde_json::Value = serde_json::from_str(&text)?;
            if resp.get("type").and_then(|v| v.as_str()).map_or(false, |t| t.ends_with("_ack")) {
                return Ok::<_, anyhow::Error>(resp.get("payload").cloned().unwrap_or_default());
            }
        }
        anyhow::bail!("No ack received")
    }).await.context("Timeout waiting for orchestrator response")??;

    Ok(response)
}
