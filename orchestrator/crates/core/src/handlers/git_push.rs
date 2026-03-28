use std::process::Command;

/// Execute a git push from the host side of a workspace.
/// Called after permission checks and human approval.
pub fn git_push(workspace_path: &str, remote: &str, branch: &str) -> Result<String, String> {
    let output = Command::new("git")
        .args(["-C", workspace_path, "push", remote, branch])
        .output()
        .map_err(|e| format!("Failed to execute git push: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{}{}", stdout, stderr);

    if output.status.success() {
        Ok(combined)
    } else {
        Err(format!("git push failed: {}", combined))
    }
}

/// Get the current branch of a workspace.
pub fn current_branch(workspace_path: &str) -> Result<String, String> {
    let output = Command::new("git")
        .args(["-C", workspace_path, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .map_err(|e| format!("Failed to get current branch: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err("Failed to determine current branch".to_string())
    }
}
