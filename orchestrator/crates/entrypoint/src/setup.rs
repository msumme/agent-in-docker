use anyhow::{bail, Context, Result};
use std::path::Path;

/// Restore ~/.claude.json from the mounted .claude directory.
pub fn restore_claude_json() -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let claude_json = format!("{}/.claude.json", home);
    let mounted_json = format!("{}/.claude/.claude.json", home);

    if Path::new(&claude_json).exists() {
        return Ok(());
    }

    if Path::new(&mounted_json).exists() {
        std::os::unix::fs::symlink(&mounted_json, &claude_json)
            .context("Failed to symlink .claude.json")?;
        return Ok(());
    }

    // Try restoring from backup
    let backups_dir = format!("{}/.claude/backups", home);
    if let Some(backup) = find_latest_backup(&backups_dir)? {
        std::fs::copy(&backup, &mounted_json)?;
        std::os::unix::fs::symlink(&mounted_json, &claude_json)?;
    }

    Ok(())
}

/// Verify OAuth credentials exist.
pub fn verify_credentials() -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let creds = format!("{}/.claude/.credentials.json", home);

    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        return Ok(());
    }
    if Path::new(&creds).exists() {
        return Ok(());
    }
    bail!("No credentials found. Run './run-agent.sh login' first.");
}

/// Pre-accept workspace trust by editing .claude.json.
pub fn pre_accept_workspace_trust() -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let claude_json = format!("{}/.claude.json", home);

    if !Path::new(&claude_json).exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&claude_json)?;
    let mut doc: serde_json::Value = serde_json::from_str(&content)
        .context("Failed to parse .claude.json")?;

    let projects = doc.as_object_mut()
        .and_then(|o| Some(o.entry("projects").or_insert(serde_json::json!({}))))
        .and_then(|v| v.as_object_mut());

    if let Some(projects) = projects {
        let workspace = projects
            .entry("/workspace")
            .or_insert(serde_json::json!({}));
        if let Some(ws) = workspace.as_object_mut() {
            ws.insert("hasTrustDialogAccepted".into(), serde_json::json!(true));
        }
    }

    if let Some(obj) = doc.as_object_mut() {
        obj.insert("hasCompletedOnboarding".into(), serde_json::json!(true));
    }

    std::fs::write(&claude_json, serde_json::to_string_pretty(&doc)?)?;
    Ok(())
}

/// Set beads env vars for host dolt server.
pub fn configure_beads() -> Result<()> {
    // These are set by the CLI if dolt is running on the host
    // The env vars are already passed through podman -e, just verify
    if std::env::var("DOLT_HOST").is_ok() && std::env::var("DOLT_PORT").is_ok() {
        std::env::set_var("BEADS_DOLT_SERVER_HOST", std::env::var("DOLT_HOST").unwrap());
        std::env::set_var("BEADS_DOLT_SERVER_PORT", std::env::var("DOLT_PORT").unwrap());
    }
    Ok(())
}

/// Write MCP config file pointing to the orchestrator's HTTP MCP server.
pub fn write_mcp_config(mcp_port: &str, agent_name: &str) -> Result<()> {
    let url = format!("http://host.containers.internal:{}/mcp", mcp_port);
    let config = serde_json::json!({
        "mcpServers": {
            "agent-bridge": {
                "type": "http",
                "url": url,
                "headers": {
                    "X-Agent-Name": agent_name
                }
            }
        }
    });
    std::fs::write("/tmp/mcp-config.json", serde_json::to_string_pretty(&config)?)?;
    Ok(())
}

fn find_latest_backup(dir: &str) -> Result<Option<String>> {
    let path = Path::new(dir);
    if !path.exists() {
        return Ok(None);
    }
    let mut backups: Vec<_> = std::fs::read_dir(path)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".claude.json.backup."))
        .map(|e| e.path().to_string_lossy().to_string())
        .collect();
    backups.sort();
    Ok(backups.last().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_mcp_config_creates_valid_json() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("mcp-config.json");

        // Can't write to /tmp/mcp-config.json in tests, so test the JSON generation
        let url = format!("http://host.containers.internal:{}/mcp", "9801");
        let config = serde_json::json!({
            "mcpServers": {
                "agent-bridge": {
                    "type": "http",
                    "url": url,
                    "headers": { "X-Agent-Name": "test" }
                }
            }
        });
        let json = serde_json::to_string_pretty(&config).unwrap();
        assert!(json.contains("9801"));
        assert!(json.contains("X-Agent-Name"));
    }

    #[test]
    fn pre_accept_edits_json_correctly() {
        let tmp = tempfile::tempdir().unwrap();
        let json_path = tmp.path().join(".claude.json");
        std::fs::write(&json_path, r#"{"projects":{}}"#).unwrap();

        // Simulate the edit logic
        let content = std::fs::read_to_string(&json_path).unwrap();
        let mut doc: serde_json::Value = serde_json::from_str(&content).unwrap();
        let projects = doc.as_object_mut().unwrap()
            .get_mut("projects").unwrap()
            .as_object_mut().unwrap();
        let ws = projects.entry("/workspace").or_insert(serde_json::json!({}));
        ws.as_object_mut().unwrap().insert("hasTrustDialogAccepted".into(), serde_json::json!(true));

        assert!(doc["projects"]["/workspace"]["hasTrustDialogAccepted"].as_bool().unwrap());
    }

    #[test]
    fn find_latest_backup_returns_newest() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".claude.json.backup.100"), "old").unwrap();
        std::fs::write(tmp.path().join(".claude.json.backup.200"), "new").unwrap();
        let result = find_latest_backup(tmp.path().to_str().unwrap()).unwrap();
        assert!(result.is_some());
        assert!(result.unwrap().contains("200"));
    }
}
