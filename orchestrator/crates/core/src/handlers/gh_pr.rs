use std::io::Write;
use std::process::Command;

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrLifecycle {
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone)]
pub struct PrState {
    pub state: PrLifecycle,
    pub merge_commit: Option<String>,
}

/// Query the current state of a PR by number. Used by RealGhClient.
pub fn pr_state(workspace: &str, number: u64) -> Result<PrState, String> {
    let mut cmd = Command::new("gh");
    if !workspace.is_empty() {
        cmd.args(["-C", workspace]);
    }
    cmd.args([
        "pr",
        "view",
        &number.to_string(),
        "--json",
        "state,mergeCommit",
    ]);

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to execute gh pr view: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!("gh pr view failed: {}{}", stdout, stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: Value = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("Failed to parse gh pr view output: {} (raw: {})", e, stdout))?;

    let lifecycle = match v.get("state").and_then(|s| s.as_str()) {
        Some("MERGED") => PrLifecycle::Merged,
        Some("CLOSED") => PrLifecycle::Closed,
        _ => PrLifecycle::Open,
    };

    let merge_commit = v
        .get("mergeCommit")
        .and_then(|mc| mc.get("oid"))
        .and_then(|oid| oid.as_str())
        .map(|s| s.to_string());

    Ok(PrState {
        state: lifecycle,
        merge_commit,
    })
}

/// Create a GitHub pull request using the host's gh credentials.
/// Writes the body to a tempfile to avoid shell-escape and arg-length issues.
/// The tempfile is cleaned up when this function returns.
pub fn pr_create(
    workspace: &str,
    base: &str,
    head: &str,
    title: &str,
    body: &str,
    draft: bool,
) -> Result<(String, u64), String> {
    let mut tmp = tempfile::NamedTempFile::new()
        .map_err(|e| format!("Failed to create tempfile: {}", e))?;
    tmp.write_all(body.as_bytes())
        .map_err(|e| format!("Failed to write body to tempfile: {}", e))?;
    tmp.flush()
        .map_err(|e| format!("Failed to flush tempfile: {}", e))?;
    let tmp_path = tmp.path().to_str().unwrap().to_string();

    let mut cmd = Command::new("gh");
    if !workspace.is_empty() {
        cmd.args(["-C", workspace]);
    }
    cmd.args([
        "pr",
        "create",
        "--base",
        base,
        "--head",
        head,
        "--title",
        title,
        "--body-file",
        &tmp_path,
    ]);
    if draft {
        cmd.arg("--draft");
    }

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to execute gh pr create: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // NamedTempFile deletes the file when dropped
    drop(tmp);

    if output.status.success() {
        // gh pr create prints the PR URL on its own line (typically the last
        // non-empty line of stdout). It does not support --json.
        let url = stdout
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| l.starts_with("http"))
            .unwrap_or("")
            .to_string();
        let number = url
            .rsplit('/')
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        Ok((url, number))
    } else {
        Err(format!("gh pr create failed: {}{}", stdout, stderr))
    }
}

/// View details of a GitHub pull request.
/// Returns the raw JSON value from gh.
pub fn pr_view(workspace: &str, ref_: &str) -> Result<Value, String> {
    let mut cmd = Command::new("gh");
    if !workspace.is_empty() {
        cmd.args(["-C", workspace]);
    }
    cmd.args([
        "pr",
        "view",
        ref_,
        "--json",
        "number,url,title,body,state,author,baseRefName,headRefName,createdAt,updatedAt",
    ]);

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to execute gh pr view: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        serde_json::from_str(stdout.trim()).map_err(|e| {
            format!(
                "Failed to parse gh pr view output: {} (raw: {})",
                e, stdout
            )
        })
    } else {
        Err(format!("gh pr view failed: {}{}", stdout, stderr))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) static PATH_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn make_fake_gh(script: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let gh_path = dir.path().join("gh");
        std::fs::write(&gh_path, format!("#!/bin/sh\n{}", script)).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    pub(crate) fn with_fake_gh<T>(dir: &tempfile::TempDir, f: impl FnOnce() -> T) -> T {
        let _guard = PATH_MUTEX.lock().unwrap();
        let dir_path = dir.path().to_str().unwrap();
        let original = std::env::var("PATH").unwrap_or_default();
        // SAFETY: single-threaded via PATH_MUTEX
        unsafe { std::env::set_var("PATH", format!("{}:{}", dir_path, original)) };
        let result = f();
        unsafe { std::env::set_var("PATH", original) };
        result
    }

    #[test]
    fn pr_create_parses_url_and_number() {
        let dir = make_fake_gh(
            r#"printf 'https://github.com/owner/repo/pull/1\n'"#,
        );
        let result = with_fake_gh(&dir, || pr_create("", "main", "feat", "Title", "Body", false));
        let (url, number) = result.unwrap();
        assert_eq!(url, "https://github.com/owner/repo/pull/1");
        assert_eq!(number, 1);
    }

    #[test]
    fn pr_create_returns_err_on_nonzero_exit() {
        let dir = make_fake_gh("echo 'some error' >&2; exit 1");
        let result = with_fake_gh(&dir, || pr_create("", "main", "feat", "Title", "Body", false));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed"));
    }

    #[test]
    fn pr_create_tempfile_cleaned_up_after_call() {
        let capture = tempfile::NamedTempFile::new().unwrap();
        let capture_path = capture.path().to_str().unwrap().to_string();

        let script = format!(
            r#"
prev=""
for arg in "$@"; do
  if [ "$prev" = "bf" ]; then
    printf '%s' "$arg" > {capture}
  fi
  if [ "$arg" = "--body-file" ]; then prev="bf"; else prev=""; fi
done
printf 'https://github.com/owner/repo/pull/1\n'
"#,
            capture = capture_path
        );
        let dir = make_fake_gh(&script);

        let result = with_fake_gh(&dir, || pr_create("", "main", "feat", "Title", "Body", false));
        assert!(result.is_ok());

        let body_file_path = std::fs::read_to_string(&capture_path).unwrap();
        assert!(!body_file_path.is_empty(), "Should have captured body-file path");
        assert!(
            !std::path::Path::new(body_file_path.trim()).exists(),
            "Body tempfile should be cleaned up after pr_create returns"
        );
    }

    #[test]
    fn pr_view_returns_json_on_success() {
        let dir = make_fake_gh(
            r#"printf '{"number":42,"url":"https://github.com/o/r/pull/42","title":"My PR","state":"OPEN"}'"#,
        );
        let result = with_fake_gh(&dir, || pr_view("", "42"));
        let v = result.unwrap();
        assert_eq!(v["number"], 42);
        assert_eq!(v["title"], "My PR");
    }

    #[test]
    fn pr_view_returns_err_on_nonzero_exit() {
        let dir = make_fake_gh("echo 'no pr found' >&2; exit 1");
        let result = with_fake_gh(&dir, || pr_view("", "999"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed"));
    }
}
