use anyhow::{bail, Context, Result};
use std::process::Command;

use crate::config::Config;
use orchestrator_core::agent_manager::ShellOps;
use orchestrator_core::types::StartAgentPayload;

pub fn image_exists(name: &str) -> Result<bool> {
    let status = Command::new("podman")
        .args(["image", "exists", name])
        .status()
        .context("Failed to run podman")?;
    Ok(status.success())
}

pub fn build_image(cfg: &Config) -> Result<()> {
    let status = Command::new("podman")
        .args([
            "build",
            "-f",
            &cfg.containerfile.to_string_lossy(),
            "-t",
            &cfg.image_name,
            &cfg.project_root.to_string_lossy(),
        ])
        .status()
        .context("Failed to build container image")?;
    if !status.success() {
        bail!("Container image build failed");
    }
    Ok(())
}

pub fn ensure_network(name: &str) -> Result<()> {
    let _ = Command::new("podman")
        .args(["network", "create", name])
        .output();
    Ok(())
}

/// Write a shell script that runs the podman command (for tmux to execute).
fn write_run_script(cfg: &StartAgentPayload, script_path: &str) -> Result<()> {
    let args = cfg.container_run_args();
    let quoted_args: Vec<String> = args
        .iter()
        .map(|a| {
            if a.contains(' ') || a.contains('\'') {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.clone()
            }
        })
        .collect();

    let script = format!(
        "#!/bin/bash\npodman run -it {}\necho '[Agent exited. Press Enter to close.]'\nread\n",
        quoted_args.join(" \\\n    ")
    );
    std::fs::write(script_path, &script)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(script_path, std::fs::Permissions::from_mode(0o755))?;
    }

    Ok(())
}

pub fn launch_long_running(cfg: &StartAgentPayload) -> Result<()> {
    let script_path = format!(
        "{}/../run-{}.sh",
        cfg.agent_dir, cfg.name
    );
    write_run_script(cfg, &script_path)?;

    // Agents run in the same tmux session as the orchestrator
    // so you can switch between TUI and agents with Ctrl-b n/p
    let tmux_session = "orchestrator";

    // Create or add to tmux session
    let has_session = Command::new("tmux")
        .args(["has-session", "-t", tmux_session])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if has_session {
        let target = format!("{}:", tmux_session);
        let status = Command::new("tmux")
            .args(["new-window", "-t", &target, "-n", &cfg.name, &script_path])
            .status()?;
        if !status.success() {
            bail!("Failed to create tmux window");
        }
    } else {
        let status = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                tmux_session,
                "-n",
                &cfg.name,
                &script_path,
            ])
            .status()?;
        if !status.success() {
            bail!("Failed to create tmux session");
        }
    }

    println!("==> Agent '{}' started", cfg.name);
    println!("    Attach:      tmux attach -t orchestrator");
    println!("    Switch:      Ctrl-b n (next) / Ctrl-b p (prev)");
    println!("    Detach:      Ctrl-b d");

    // Auto-accept bypass permissions dialog and send initial prompt.
    let shell = orchestrator_core::agent_manager::RealShellOps;
    if !cfg.prompt.is_empty() {
        let _ = shell.write_prompt_file(&cfg.name, &cfg.prompt);
    }
    let target = format!("orchestrator:{}", cfg.name);
    let script = orchestrator_core::agent_manager::auto_accept_script(&target, &cfg.name);
    let _ = shell.spawn_background_script(&script);

    Ok(())
}

pub fn launch_oneshot(cfg: &StartAgentPayload) -> Result<()> {
    println!("==> Launching agent container...");
    let mut args = vec!["run".to_string(), "-it".to_string()];
    args.extend(cfg.container_run_args());

    let status = Command::new("podman")
        .args(&args)
        .status()
        .context("Failed to launch container")?;

    if !status.success() {
        bail!("Container exited with error");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_payload() -> StartAgentPayload {
        StartAgentPayload {
            name: "test".into(),
            project_path: "/tmp/project".into(),
            agent_dir: "/tmp/agent".into(),
            role: "code-agent".into(),
            mode: "oneshot".into(),
            prompt: "hello".into(),
            orchestrator_port: 9800,
            mcp_port: 9801,
            dolt_port: None,
            seed_credentials: "/tmp/creds.json".into(),
            image_name: "agent-in-docker".into(),
            network_name: "agent-net".into(),
        }
    }

    #[test]
    fn container_run_args_contains_required_env() {
        let cfg = make_payload();
        let args = cfg.container_run_args();
        assert!(args.iter().any(|a| a == "AGENT_NAME=test"));
        assert!(args.iter().any(|a| a == "AGENT_MODE=oneshot"));
        assert!(args.iter().any(|a| a == "MCP_PORT=9801"));
        assert!(args.iter().any(|a| a == "agent-in-docker"));
    }

    #[test]
    fn write_run_script_creates_executable() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("run.sh");
        let mut cfg = make_payload();
        cfg.mode = "long-running".into();
        cfg.prompt = "hi there".into();

        write_run_script(&cfg, script.to_str().unwrap()).unwrap();

        let content = std::fs::read_to_string(&script).unwrap();
        assert!(content.starts_with("#!/bin/bash"));
        assert!(content.contains("podman run -it"));
        assert!(content.contains("AGENT_PROMPT=hi there"));
    }
}
