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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_git_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        Command::new("git").args(["-C", path, "init"]).output().unwrap();
        Command::new("git").args(["-C", path, "config", "user.email", "test@test.com"]).output().unwrap();
        Command::new("git").args(["-C", path, "config", "user.name", "test"]).output().unwrap();
        std::fs::write(dir.path().join("file.txt"), "hello").unwrap();
        Command::new("git").args(["-C", path, "add", "."]).output().unwrap();
        Command::new("git").args(["-C", path, "commit", "-m", "init"]).output().unwrap();
        dir
    }

    #[test]
    fn current_branch_returns_main_or_master() {
        let dir = make_git_repo();
        let branch = current_branch(dir.path().to_str().unwrap()).unwrap();
        assert!(branch == "main" || branch == "master");
    }

    #[test]
    fn current_branch_fails_for_nonexistent_dir() {
        let result = current_branch("/nonexistent/path");
        assert!(result.is_err());
    }

    #[test]
    fn git_push_fails_without_remote() {
        let dir = make_git_repo();
        let result = git_push(dir.path().to_str().unwrap(), "origin", "main");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed"));
    }
}
