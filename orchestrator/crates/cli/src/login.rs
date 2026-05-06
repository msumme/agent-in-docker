use anyhow::{Context, Result};
use std::process::Command;

use crate::config::Config;

pub fn run_login(cfg: &Config) -> Result<()> {
    println!("==> Starting Claude Code login flow...");

    std::fs::create_dir_all(&cfg.seed_dir)?;

    // Try keychain extraction first — if the host's Claude Code is logged
    // in, this completes with zero browser interaction (after a one-time
    // "Always Allow" on the macOS keychain prompt).
    match crate::auth::refresh_credentials_from_keychain(&cfg.seed_dir) {
        Ok(true) => {
            println!(
                "==> Credentials refreshed from macOS keychain. No browser flow needed.\n\
                 ==> Wrote {}",
                cfg.seed_dir.join(".credentials.json").display()
            );
            return Ok(());
        }
        Ok(false) => println!(
            "==> Keychain entry not accessible — falling back to interactive login."
        ),
        Err(e) => eprintln!("==> Warning: keychain extraction failed ({}); falling back.", e),
    }

    // Ensure container image exists for interactive fallback
    if !crate::container::image_exists(&cfg.image_name)? {
        println!("==> Building container image first...");
        crate::container::build_image(cfg)?;
    }

    // Restore .claude.json from backup if missing
    let claude_json = cfg.seed_dir.join(".claude.json");
    if !claude_json.exists() {
        let backups_dir = cfg.seed_dir.join("backups");
        if let Some(backup) = orchestrator_core::project_config::find_latest_backup(&backups_dir)? {
            std::fs::copy(&backup, &claude_json)?;
            println!("==> Restored .claude.json from backup");
        }
    }

    // Run Claude Code interactively -- user types /login inside
    println!("==> Starting Claude Code. Type /login to authenticate.");
    let status = Command::new("podman")
        .args([
            "run",
            "-it",
            "--rm",
            "--entrypoint",
            "claude",
            "-e", "IS_SANDBOX=1",
            "-w", "/tmp",
            "-v",
            &format!("{}:/root/.claude:Z", cfg.seed_dir.display()),
            &cfg.image_name,
            "--dangerously-skip-permissions",
        ])
        .status()
        .context("Failed to run login container")?;

    if cfg.seed_dir.join(".credentials.json").exists() {
        println!("==> Login successful! Credentials saved.");
    } else if status.success() {
        println!("==> Session ended. Use /login inside Claude Code to authenticate.");
    } else {
        println!("==> Login may have failed. Check {} for credentials.", cfg.seed_dir.display());
    }

    Ok(())
}
