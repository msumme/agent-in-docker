use anyhow::{Context, Result};
use std::process::Command;

use crate::config::Config;

pub fn run_login(cfg: &Config) -> Result<()> {
    println!("==> Starting Claude Code login flow...");

    // Ensure container image exists
    if !crate::container::image_exists(&cfg.image_name)? {
        println!("==> Building container image first...");
        crate::container::build_image(cfg)?;
    }

    std::fs::create_dir_all(&cfg.seed_dir)?;

    // Restore .claude.json from backup if missing
    let claude_json = cfg.seed_dir.join(".claude.json");
    if !claude_json.exists() {
        let backups_dir = cfg.seed_dir.join("backups");
        if backups_dir.exists() {
            if let Some(backup) = find_latest_backup(&backups_dir)? {
                std::fs::copy(&backup, &claude_json)?;
                println!("==> Restored .claude.json from backup");
            }
        }
    }

    // Run claude login interactively
    let status = Command::new("podman")
        .args([
            "run",
            "-it",
            "--rm",
            "--entrypoint",
            "bash",
            "-w",
            "/tmp",
            "-v",
            &format!("{}:/root/.claude:Z", cfg.seed_dir.display()),
            &cfg.image_name,
            "-c",
            r#"
                if [ -f ~/.claude/.claude.json ] && [ ! -f ~/.claude.json ]; then
                    ln -s ~/.claude/.claude.json ~/.claude.json
                fi
                # Pre-accept trust for /tmp
                if [ -f ~/.claude.json ]; then
                    node -e "
                      const fs = require('fs');
                      const d = JSON.parse(fs.readFileSync(process.env.HOME + '/.claude.json'));
                      if (!d.projects) d.projects = {};
                      d.projects['/tmp'] = {hasTrustDialogAccepted: true};
                      d.hasCompletedOnboarding = true;
                      fs.writeFileSync(process.env.HOME + '/.claude.json', JSON.stringify(d));
                    " 2>/dev/null || true
                fi
                claude
            "#,
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

fn find_latest_backup(dir: &std::path::Path) -> Result<Option<std::path::PathBuf>> {
    let mut backups: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".claude.json.backup.")
        })
        .map(|e| e.path())
        .collect();
    backups.sort();
    Ok(backups.last().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_latest_backup_returns_newest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".claude.json.backup.100"), "old").unwrap();
        std::fs::write(dir.path().join(".claude.json.backup.200"), "new").unwrap();
        std::fs::write(dir.path().join("other-file"), "ignore").unwrap();

        let result = find_latest_backup(dir.path()).unwrap();
        assert!(result.is_some());
        assert!(result.unwrap().to_str().unwrap().contains("200"));
    }

    #[test]
    fn find_latest_backup_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = find_latest_backup(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn find_latest_backup_no_matching_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("random.txt"), "data").unwrap();
        let result = find_latest_backup(dir.path()).unwrap();
        assert!(result.is_none());
    }
}
