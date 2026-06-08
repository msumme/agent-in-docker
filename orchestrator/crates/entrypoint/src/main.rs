mod setup;
mod register;

use anyhow::{bail, Result};
use std::process::Command;

/// Pure assembly of Claude Code CLI arguments from resolved inputs.
/// Keeps `main` free of arg-building logic and makes the rules testable.
fn build_claude_args(
    role_prompt: &str,
    model: Option<String>,
    effort: Option<String>,
    mode: &str,
    prompt: &str,
    resume: bool,
) -> Vec<String> {
    let mut args = vec![
        "--dangerously-skip-permissions".to_string(),
        "--mcp-config".to_string(),
        "/tmp/mcp-config.json".to_string(),
    ];

    if !role_prompt.is_empty() {
        args.push("--append-system-prompt".to_string());
        args.push(role_prompt.to_string());
    }

    if let Some(m) = model.filter(|s| !s.is_empty()) {
        args.push("--model".to_string());
        args.push(m);
    }
    if let Some(e) = effort.filter(|s| !s.is_empty()) {
        args.push("--effort".to_string());
        args.push(e);
    }

    if resume && mode == "long-running" {
        args.push("--continue".to_string());
    }

    if mode == "oneshot" && !prompt.is_empty() {
        args.push("-p".to_string());
        args.push(prompt.to_string());
    }

    args
}

#[tokio::main]
async fn main() -> Result<()> {
    let agent_name = std::env::var("AGENT_NAME").unwrap_or_else(|_| "unnamed".into());
    let agent_role = std::env::var("AGENT_ROLE").unwrap_or_else(|_| "code-agent".into());
    let agent_mode = std::env::var("AGENT_MODE").unwrap_or_else(|_| "oneshot".into());
    let agent_prompt = std::env::var("AGENT_PROMPT").unwrap_or_default();
    let agent_role_prompt = std::env::var("AGENT_ROLE_PROMPT").unwrap_or_default();
    let orchestrator_url = std::env::var("ORCHESTRATOR_URL").unwrap_or_else(|_| "ws://host.containers.internal:9800".into());
    let mcp_port = std::env::var("MCP_PORT").unwrap_or_else(|_| "9801".into());
    let resume = std::env::var("AGENT_RESUME").map(|v| v == "1").unwrap_or(false);

    eprintln!("[entrypoint] {} ({}, {})", agent_name, agent_role, agent_mode);

    // Setup credentials and config
    setup::restore_claude_json()?;
    setup::verify_credentials()?;
    setup::pre_accept_workspace_trust()?;
    setup::configure_beads()?;
    setup::configure_git_identity(&agent_name)?;
    setup::write_mcp_config(&mcp_port, &agent_name, &agent_role)?;

    // Register with orchestrator via WebSocket (background task)
    let ws_handle = tokio::spawn(register::register_and_stay_connected(
        orchestrator_url,
        agent_name.clone(),
        agent_role.clone(),
    ));

    // Give registration a moment to connect
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    if !agent_role_prompt.is_empty() {
        eprintln!(
            "[entrypoint] Appending system prompt ({} chars, first line: {:?})",
            agent_role_prompt.len(),
            agent_role_prompt.lines().next().unwrap_or("")
        );
    } else {
        eprintln!("[entrypoint] No AGENT_ROLE_PROMPT set — starting without --append-system-prompt");
    }

    let model = std::env::var("AGENT_MODEL").ok().filter(|s| !s.is_empty());
    let effort = std::env::var("AGENT_EFFORT").ok().filter(|s| !s.is_empty());
    if let Some(ref m) = model {
        eprintln!("[entrypoint] --model {}", m);
    }
    if let Some(ref e) = effort {
        eprintln!("[entrypoint] --effort {}", e);
    }
    if resume {
        eprintln!("[entrypoint] --continue (resume mode)");
    }

    let claude_args = build_claude_args(&agent_role_prompt, model, effort, &agent_mode, &agent_prompt, resume);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_claude_args_continue_present_iff_resume_and_long_running() {
        let args = build_claude_args("", None, None, "long-running", "", true);
        assert!(args.iter().any(|a| a == "--continue"), "must have --continue for resume+long-running");
    }

    #[test]
    fn build_claude_args_no_continue_when_not_resume() {
        let args = build_claude_args("", None, None, "long-running", "", false);
        assert!(!args.iter().any(|a| a == "--continue"), "--continue must be absent when resume=false");
    }

    #[test]
    fn build_claude_args_no_continue_for_oneshot_even_when_resume() {
        let args = build_claude_args("", None, None, "oneshot", "do it", true);
        assert!(!args.iter().any(|a| a == "--continue"), "--continue must be absent for oneshot even with resume=true");
    }

    #[test]
    fn build_claude_args_oneshot_ends_with_prompt() {
        let args = build_claude_args("", None, None, "oneshot", "my-task", false);
        let tail: Vec<_> = args.iter().rev().take(2).collect();
        assert_eq!(tail[0], "my-task", "last arg must be the prompt");
        assert_eq!(tail[1], "-p", "second-to-last arg must be -p");
    }

    #[test]
    fn build_claude_args_oneshot_no_prompt_when_empty() {
        let args = build_claude_args("", None, None, "oneshot", "", false);
        assert!(!args.iter().any(|a| a == "-p"), "-p must be absent when prompt is empty");
    }

    #[test]
    fn build_claude_args_role_prompt_appended() {
        let args = build_claude_args("my-prompt", None, None, "long-running", "", false);
        let idx = args.iter().position(|a| a == "--append-system-prompt").expect("must have --append-system-prompt");
        assert_eq!(args[idx + 1], "my-prompt");
    }

    #[test]
    fn build_claude_args_no_role_prompt_when_empty() {
        let args = build_claude_args("", None, None, "long-running", "", false);
        assert!(!args.iter().any(|a| a == "--append-system-prompt"), "--append-system-prompt must be absent for empty role_prompt");
    }

    #[test]
    fn build_claude_args_model_and_effort_included() {
        let args = build_claude_args("", Some("claude-opus-4-7".into()), Some("high".into()), "long-running", "", false);
        let model_idx = args.iter().position(|a| a == "--model").expect("--model must be present");
        assert_eq!(args[model_idx + 1], "claude-opus-4-7");
        let effort_idx = args.iter().position(|a| a == "--effort").expect("--effort must be present");
        assert_eq!(args[effort_idx + 1], "high");
    }
}
