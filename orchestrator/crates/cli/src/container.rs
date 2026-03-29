use anyhow::{bail, Context, Result};
use std::process::Command;

use crate::config::Config;

pub struct RunConfig {
    pub agent_name: String,
    pub project_path: String,
    pub agent_dir: String,
    pub role: String,
    pub mode: String,
    pub prompt: String,
    pub orchestrator_port: u16,
    pub mcp_port: u16,
    pub image_name: String,
    pub network_name: String,
}

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
            cfg.containerfile.to_str().unwrap(),
            "-t",
            &cfg.image_name,
            cfg.project_root.to_str().unwrap(),
        ])
        .status()
        .context("Failed to build container image")?;
    if !status.success() {
        bail!("Container image build failed");
    }
    Ok(())
}

/// Detect the port of a running dolt sql-server on the host.
fn detect_dolt_port() -> Option<u16> {
    let output = Command::new("lsof")
        .args(["-iTCP", "-sTCP:LISTEN", "-P", "-n"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains("dolt") {
            // Parse port from "TCP *:52837 (LISTEN)" or "TCP 127.0.0.1:52837 (LISTEN)"
            if let Some(addr_part) = line.split_whitespace().find(|s| s.contains(':') && s.chars().last().map_or(false, |c| c.is_ascii_digit())) {
                if let Some(port_str) = addr_part.rsplit(':').next() {
                    return port_str.parse().ok();
                }
            }
        }
    }
    None
}

pub fn ensure_network(name: &str) -> Result<()> {
    let _ = Command::new("podman")
        .args(["network", "create", name])
        .output();
    Ok(())
}

fn podman_run_args(cfg: &RunConfig) -> Vec<String> {
    let mut args = vec![
        "--rm".to_string(),
        "--name".to_string(),
        cfg.agent_name.clone(),
        "--network".to_string(),
        cfg.network_name.clone(),
        // Security hardening
        "--cap-drop=ALL".to_string(),
        "--cap-add=NET_RAW".to_string(),     // DNS resolution
        "--cap-add=CHOWN".to_string(),       // Entrypoint sets up agent user home
        "--cap-add=SETUID".to_string(),      // su from root to agent user
        "--cap-add=SETGID".to_string(),      // su from root to agent user
        "--cap-add=DAC_OVERRIDE".to_string(),// Create /home/agent
        // Volumes
        "-v".to_string(),
        format!("{}:/workspace:Z", cfg.project_path),
        "-v".to_string(),
        format!("{}:/root/.claude:Z", cfg.agent_dir),
        "-e".to_string(),
        format!(
            "ORCHESTRATOR_URL=ws://host.containers.internal:{}",
            cfg.orchestrator_port
        ),
        "-e".to_string(),
        format!("MCP_PORT={}", cfg.mcp_port),
        "-e".to_string(),
        format!("AGENT_NAME={}", cfg.agent_name),
        "-e".to_string(),
        format!("AGENT_ROLE={}", cfg.role),
        "-e".to_string(),
        format!("AGENT_MODE={}", cfg.mode),
        "-e".to_string(),
        format!("AGENT_PROMPT={}", cfg.prompt),
    ];

    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            args.extend_from_slice(&["-e".to_string(), format!("ANTHROPIC_API_KEY={}", key)]);
        }
    }


    args.push(cfg.image_name.clone());
    args
}

/// Write a shell script that runs the podman command (for tmux to execute).
fn write_run_script(cfg: &RunConfig, script_path: &str) -> Result<()> {
    let args = podman_run_args(cfg);
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

pub fn launch_long_running(cfg: &RunConfig) -> Result<()> {
    let script_path = format!(
        "{}/../run-{}.sh",
        cfg.agent_dir, cfg.agent_name
    );
    write_run_script(cfg, &script_path)?;

    let tmux_session = "agents";

    // Create or add to tmux session
    let has_session = Command::new("tmux")
        .args(["has-session", "-t", tmux_session])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if has_session {
        let status = Command::new("tmux")
            .args(["new-window", "-t", tmux_session, "-n", &cfg.agent_name, &script_path])
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
                &cfg.agent_name,
                &script_path,
            ])
            .status()?;
        if !status.success() {
            bail!("Failed to create tmux session");
        }
    }

    println!("==> Agent '{}' started in tmux session 'agents'", cfg.agent_name);
    println!("    Attach to agent:       tmux attach -t agents");
    println!("    Switch agents:         Ctrl-b n / Ctrl-b p");
    println!("    Detach:                Ctrl-b d");
    println!("    Orchestrator TUI:      tmux attach -t orchestrator");

    // Auto-accept dialogs (blocks until done)
    auto_accept_dialogs(&cfg.agent_name, &cfg.prompt);

    Ok(())
}

pub fn launch_oneshot(cfg: &RunConfig) -> Result<()> {
    println!("==> Launching agent container...");
    let mut args = vec!["run".to_string(), "-it".to_string()];
    args.extend(podman_run_args(cfg));

    let status = Command::new("podman")
        .args(&args)
        .status()
        .context("Failed to launch container")?;

    if !status.success() {
        bail!("Container exited with error");
    }
    Ok(())
}

fn auto_accept_dialogs(agent_name: &str, prompt: &str) {
    let target = format!("agents:{}", agent_name);

    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_secs(2));

        let pane = Command::new("tmux")
            .args(["capture-pane", "-t", &target, "-p"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();

        if pane.contains("Yes, I accept") {
            // Arrow down to "Yes, I accept" and press Enter
            let _ = Command::new("tmux")
                .args(["send-keys", "-t", &target, "Down"])
                .status();
            std::thread::sleep(std::time::Duration::from_secs(1));
            let _ = Command::new("tmux")
                .args(["send-keys", "-t", &target, "Enter"])
                .status();
            eprintln!("==> Auto-accepted bypass permissions dialog");
            break;
        }

        if pane.contains("\u{256d}\u{2500}") {
            // Claude Code prompt box -- no dialog to accept
            break;
        }
    }

    // Send initial prompt
    if !prompt.is_empty() {
        std::thread::sleep(std::time::Duration::from_secs(3));
        let _ = Command::new("tmux")
            .args(["send-keys", "-t", &format!("agents:{}", agent_name), prompt, "Enter"])
            .status();
        eprintln!("==> Sent initial prompt to agent");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn podman_run_args_contains_required_env() {
        let cfg = RunConfig {
            agent_name: "test".into(),
            project_path: "/tmp/project".into(),
            agent_dir: "/tmp/agent".into(),
            role: "code-agent".into(),
            mode: "oneshot".into(),
            prompt: "hello".into(),
            orchestrator_port: 9800,
            mcp_port: 9801,
            image_name: "agent-in-docker".into(),
            network_name: "agent-net".into(),
        };

        let args = podman_run_args(&cfg);
        assert!(args.contains(&"AGENT_NAME=test".to_string()));
        assert!(args.contains(&"AGENT_MODE=oneshot".to_string()));
        assert!(args.contains(&"MCP_PORT=9801".to_string()));
        assert!(args.contains(&"agent-in-docker".to_string()));
    }

    #[test]
    fn write_run_script_creates_executable() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("run.sh");
        let cfg = RunConfig {
            agent_name: "test".into(),
            project_path: "/tmp/p".into(),
            agent_dir: "/tmp/a".into(),
            role: "code-agent".into(),
            mode: "long-running".into(),
            prompt: "hi there".into(),
            orchestrator_port: 9800,
            mcp_port: 9801,
            image_name: "agent-in-docker".into(),
            network_name: "agent-net".into(),
        };
        write_run_script(&cfg, script.to_str().unwrap()).unwrap();

        let content = std::fs::read_to_string(&script).unwrap();
        assert!(content.starts_with("#!/bin/bash"));
        assert!(content.contains("podman run -it"));
        assert!(content.contains("AGENT_PROMPT=hi there"));
    }
}
