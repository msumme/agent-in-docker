//! Auth helpers for refreshing the seed credentials non-interactively.
//!
//! On macOS, the host's Claude Code stores the OAuth blob in the user's
//! login keychain under `Claude Code-credentials`. We can read it with
//! `security find-generic-password -w` and dump it into the seed
//! `.credentials.json` — eliminating the browser-based `/login` flow as
//! long as the host's Claude Code session is healthy.
//!
//! Linux falls back to whatever is already in the seed dir; the host
//! tooling there typically writes a creds file directly.

use std::path::Path;
use std::process::Command;

/// Try to refresh `<seed_dir>/.credentials.json` from the host's Claude Code
/// keychain entry. Returns `Ok(true)` if creds were refreshed, `Ok(false)`
/// if the platform isn't macOS or the keychain entry isn't accessible
/// (e.g. user denied the keychain prompt). Errors only on filesystem
/// failures after a successful extraction.
pub fn refresh_credentials_from_keychain(seed_dir: &Path) -> Result<bool, String> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }

    let out = Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials", "-w"])
        .output()
        .map_err(|e| format!("invoke security: {}", e))?;

    if !out.status.success() {
        // Keychain entry missing, locked, or denied. Caller should fall back
        // to whatever creds are already in the seed dir.
        return Ok(false);
    }

    let blob = String::from_utf8(out.stdout)
        .map_err(|e| format!("keychain blob is not utf-8: {}", e))?;
    let trimmed = blob.trim();

    // Sanity check: the blob should be a JSON object containing an
    // `claudeAiOauth` field. If it's empty or obviously wrong, refuse to
    // overwrite the seed file — better to fall back than corrupt it.
    if trimmed.is_empty() || !trimmed.starts_with('{') {
        return Ok(false);
    }

    std::fs::create_dir_all(seed_dir)
        .map_err(|e| format!("create seed dir {}: {}", seed_dir.display(), e))?;
    let path = seed_dir.join(".credentials.json");
    std::fs::write(&path, trimmed)
        .map_err(|e| format!("write {}: {}", path.display(), e))?;

    // OAuth tokens are sensitive; chmod 600 to match what `claude /login` writes.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(true)
}
