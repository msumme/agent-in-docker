mod setup;
mod register;

use anyhow::{bail, Result};
use std::process::Command;

#[tokio::main]
async fn main() -> Result<()> {
    let agent_name = std::env::var("AGENT_NAME").unwrap_or_else(|_| "unnamed".into());
    let agent_role = std::env::var("AGENT_ROLE").unwrap_or_else(|_| "code-agent".into());
    let agent_mode = std::env::var("AGENT_MODE").unwrap_or_else(|_| "oneshot".into());
    let agent_prompt = std::env::var("AGENT_PROMPT").unwrap_or_default();
    let orchestrator_url = std::env::var("ORCHESTRATOR_URL").unwrap_or_else(|_| "ws://host.containers.internal:9800".into());
    let mcp_port = std::env::var("MCP_PORT").unwrap_or_else(|_| "9801".into());

    eprintln!("[entrypoint] {} ({}, {})", agent_name, agent_role, agent_mode);

    // Setup credentials and config
    setup::restore_claude_json()?;
    setup::verify_credentials()?;
    setup::pre_accept_workspace_trust()?;
    setup::configure_beads()?;
    setup::write_mcp_config(&mcp_port, &agent_name, &agent_role)?;

    // Register with orchestrator via WebSocket (background task)
    let ws_handle = tokio::spawn(register::register_and_stay_connected(
        orchestrator_url,
        agent_name.clone(),
        agent_role.clone(),
    ));

    // Give registration a moment to connect
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Launch Claude Code
    let mut claude_args = vec![
        "--dangerously-skip-permissions".to_string(),
        "--mcp-config".to_string(),
        "/tmp/mcp-config.json".to_string(),
    ];

    if agent_mode == "oneshot" && !agent_prompt.is_empty() {
        claude_args.push("-p".to_string());
        claude_args.push(agent_prompt);
    }

    eprintln!("[entrypoint] Starting Claude Code...");
    let status = Command::new("claude")
        .args(&claude_args)
        .env("IS_SANDBOX", "1")
        .status()?;

    // Cancel WS registration when Claude exits
    ws_handle.abort();

    if !status.success() {
        bail!("Claude Code exited with error");
    }
    Ok(())
}
