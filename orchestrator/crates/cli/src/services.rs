use anyhow::{bail, Context, Result};
use std::process::Command;

use crate::config::Config;

/// Check if the orchestrator is running by trying to connect to its port.
/// More reliable than PID file checks (no race conditions, no stale PIDs).
fn is_port_listening(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", port).parse().unwrap(),
        std::time::Duration::from_secs(1),
    )
    .is_ok()
}

pub fn ensure_orchestrator(cfg: &Config) -> Result<()> {
    if is_port_listening(cfg.orchestrator_port) {
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

/// Ensure the project's dolt server is running. Reads the port from
/// .beads/dolt-server.port in the project directory. If dolt isn't running,
/// starts it via `bd dolt start`.
pub fn ensure_dolt(project_path: &std::path::Path) -> Result<Option<u16>> {
    let port_file = project_path.join(".beads/dolt-server.port");
    if !port_file.exists() {
        // No beads database in this project
        return Ok(None);
    }

    let port_str = std::fs::read_to_string(&port_file)
        .context("Failed to read .beads/dolt-server.port")?;
    let port: u16 = port_str
        .trim()
        .parse()
        .context("Invalid port in .beads/dolt-server.port")?;

    if port == 0 {
        // Auto-detect mode -- let bd handle it, but we can't pass it to containers
        return Ok(None);
    }

    // Check if dolt is reachable on this port
    if std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", port).parse().unwrap(),
        std::time::Duration::from_secs(2),
    )
    .is_ok()
    {
        println!("==> Dolt server running on port {}", port);
        return Ok(Some(port));
    }

    // Not running -- start it
    println!("==> Starting dolt server for project...");
    let status = Command::new("bd")
        .args(["dolt", "start"])
        .current_dir(project_path)
        .status()
        .context("Failed to start dolt server")?;

    if !status.success() {
        eprintln!("Warning: dolt server failed to start (beads may not work in containers)");
        return Ok(None);
    }

    // Re-read port in case it changed
    let port_str = std::fs::read_to_string(&port_file)?;
    let port: u16 = port_str.trim().parse().unwrap_or(0);
    if port > 0 {
        println!("==> Dolt server started on port {}", port);
        Ok(Some(port))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_check_returns_false_for_unused_port() {
        assert!(!is_port_listening(19999));
    }

    #[test]
    fn port_check_returns_true_for_listening_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(is_port_listening(port));
    }
}
