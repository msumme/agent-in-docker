use anyhow::{bail, Context, Result};
use std::process::Command;

use crate::config::Config;

fn pid_is_running(pid_file: &std::path::Path) -> bool {
    if let Ok(content) = std::fs::read_to_string(pid_file) {
        if let Ok(pid) = content.trim().parse::<u32>() {
            return Command::new("kill")
                .args(["-0", &pid.to_string()])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
        }
    }
    false
}

pub fn ensure_orchestrator(cfg: &Config) -> Result<()> {
    if pid_is_running(&cfg.orchestrator_pid_file) {
        println!("==> Orchestrator already running");
        return Ok(());
    }

    if !cfg.orchestrator_bin.exists() {
        println!("==> Building orchestrator...");
        let status = Command::new("cargo")
            .args(["build"])
            .current_dir(cfg.project_root.join("orchestrator"))
            .status()
            .context("Failed to build orchestrator")?;
        if !status.success() {
            bail!("Orchestrator build failed");
        }
    }

    println!("==> Starting orchestrator on port {}...", cfg.orchestrator_port);

    let addr = format!("0.0.0.0:{}", cfg.orchestrator_port);

    if Command::new("tmux").arg("--version").output().is_ok() {
        let status = Command::new("tmux")
            .args([
                "new-session", "-d", "-s", "orchestrator",
                &format!("{} {}", cfg.orchestrator_bin.display(), addr),
            ])
            .status()?;

        if status.success() {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if let Ok(output) = Command::new("tmux")
                .args(["list-panes", "-t", "orchestrator", "-F", "#{pane_pid}"])
                .output()
            {
                let pid = String::from_utf8_lossy(&output.stdout).trim().to_string();
                std::fs::write(&cfg.orchestrator_pid_file, &pid)?;
            }
            println!("==> Orchestrator TUI in tmux session 'orchestrator'");
            println!("    Attach: tmux attach -t orchestrator");
            return Ok(());
        }
    }

    let child = Command::new(&cfg.orchestrator_bin)
        .arg(&addr)
        .spawn()
        .context("Failed to start orchestrator")?;

    std::fs::write(&cfg.orchestrator_pid_file, child.id().to_string())?;
    std::thread::sleep(std::time::Duration::from_secs(1));
    println!("==> Orchestrator started (PID: {})", child.id());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_is_running_returns_false_for_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!pid_is_running(&tmp.path().join("nonexistent.pid")));
    }

    #[test]
    fn pid_is_running_returns_false_for_invalid_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let pid_file = tmp.path().join("test.pid");
        std::fs::write(&pid_file, "99999999").unwrap();
        assert!(!pid_is_running(&pid_file));
    }
}
