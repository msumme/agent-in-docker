//! Per-role container image resolution.
//!
//! Lookup order at agent spawn (first match wins):
//!   1. `<project>/.agents/Containerfile.<role>` → image `agent-<slug>-<role>`
//!   2. `<project>/.agents/Containerfile`        → image `agent-<slug>`
//!   3. bundled `<repo>/Containerfile.base`      → image `localhost/agent-base`
//!
//! Project Containerfiles are expected to `FROM localhost/agent-base` so the
//! runtime contract (claude, bd, dolt, entrypoint) stays consistent. The
//! resolver does not enforce this — it just builds whatever the project
//! declares — but the bundled base is always built first so the FROM is
//! satisfied.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Config;

/// What to build, what to tag, where to run from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedImage {
    pub containerfile: PathBuf,
    pub image_name: String,
    pub build_context: PathBuf,
}

/// Always-present base image. Project Containerfiles `FROM` this tag.
pub const BASE_IMAGE: &str = "localhost/agent-base";

/// Resolve which Containerfile + image tag a given role should run with.
/// `project_root` is the target project (where `.agents/` lives — may differ
/// from the agent-in-docker repo when running against an external project).
/// Falls back to the bundled `agent-base` if the project has no `.agents/`.
pub fn resolve_for(project_root: &Path, role: &str) -> ResolvedImage {
    let project_agents = project_root.join(".agents");
    let role_specific = project_agents.join(format!("Containerfile.{}", role));
    let project_default = project_agents.join("Containerfile");

    let slug = project_slug(project_root);

    if role_specific.is_file() {
        return ResolvedImage {
            containerfile: role_specific,
            image_name: format!("agent-{}-{}", slug, role),
            build_context: project_root.to_path_buf(),
        };
    }
    if project_default.is_file() {
        return ResolvedImage {
            containerfile: project_default,
            image_name: format!("agent-{}", slug),
            build_context: project_root.to_path_buf(),
        };
    }
    // No project layer — use the bundled base directly.
    ResolvedImage {
        containerfile: PathBuf::new(), // unused for the base image path
        image_name: BASE_IMAGE.to_string(),
        build_context: project_root.to_path_buf(),
    }
}

/// Convenience wrapper for callers that already have a Config and want the
/// agent-in-docker repo as the project root (Team flow).
pub fn resolve(cfg: &Config, role: &str) -> ResolvedImage {
    resolve_for(&cfg.project_root, role)
}

/// Make `localhost/agent-base` available. Project images are expected to
/// `FROM` it, so it must exist before any project build runs. Idempotent —
/// no-op if the image is already present.
pub fn ensure_base_image(cfg: &Config) -> Result<()> {
    if image_exists(BASE_IMAGE)? {
        return Ok(());
    }
    let bundled = bundled_base_containerfile(cfg)?;
    println!("==> Building base image {} from {}", BASE_IMAGE, bundled.display());
    let context = bundled.parent().unwrap_or(&cfg.project_root).to_path_buf();
    podman_build(&bundled, BASE_IMAGE, &context)
}

/// Build the resolved image if it doesn't exist yet. Always ensures the base
/// image first so a FROM clause in a project Containerfile resolves locally.
pub fn ensure_image(cfg: &Config, resolved: &ResolvedImage) -> Result<()> {
    ensure_base_image(cfg)?;
    if resolved.image_name == BASE_IMAGE {
        return Ok(()); // ensure_base_image already handled it.
    }
    if image_exists(&resolved.image_name)? {
        return Ok(());
    }
    println!(
        "==> Building project image {} from {}",
        resolved.image_name,
        resolved.containerfile.display()
    );
    podman_build(
        &resolved.containerfile,
        &resolved.image_name,
        &resolved.build_context,
    )
}

fn image_exists(name: &str) -> Result<bool> {
    let status = Command::new("podman")
        .args(["image", "exists", name])
        .status()
        .context("podman not found on PATH")?;
    Ok(status.success())
}

fn podman_build(containerfile: &Path, tag: &str, context: &Path) -> Result<()> {
    let status = Command::new("podman")
        .args([
            "build",
            "-f",
            &containerfile.to_string_lossy(),
            "-t",
            tag,
            &context.to_string_lossy(),
        ])
        .status()
        .context("podman build invocation failed")?;
    if !status.success() {
        bail!("podman build of {} failed", tag);
    }
    Ok(())
}

/// `cfg.containerfile` is the bundled `Containerfile.base`; return it directly.
fn bundled_base_containerfile(cfg: &Config) -> Result<PathBuf> {
    let base = cfg.containerfile.clone();
    if !base.is_file() {
        bail!(
            "bundled base Containerfile not found at {}",
            base.display()
        );
    }
    Ok(base)
}

/// Sanitize the project directory name for use in an image tag.
/// Lowercase, [a-z0-9-] only, leading/trailing hyphens stripped.
fn project_slug(project_root: &Path) -> String {
    let raw = project_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch == '-' || ch == '_' || ch == ' ' || ch == '.' {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "project".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn cfg_with(project_root: PathBuf) -> Config {
        Config {
            seed_dir: project_root.join(".claude-container"),
            agents_dir: project_root.join(".agents"),
            orchestrator_bin: PathBuf::from("/bogus/orchestrator"),
            containerfile: project_root.join("Containerfile.base"),
            orchestrator_port: 9800,
            mcp_port: 9801,
            image_name: "ignored".into(),
            network_name: "agent-net".into(),
            orchestrator_pid_file: PathBuf::from("/tmp/bogus.pid"),
            project_root,
        }
    }

    #[test]
    fn role_specific_wins_when_present() {
        let tmp = TempDir::new().unwrap();
        let agents = tmp.path().join(".agents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(agents.join("Containerfile"), "FROM agent-base").unwrap();
        fs::write(
            agents.join("Containerfile.feature-producer"),
            "FROM agent-base\nRUN echo prod-only",
        )
        .unwrap();

        let cfg = cfg_with(tmp.path().to_path_buf());
        let r = resolve(&cfg, "feature-producer");

        assert_eq!(r.containerfile, agents.join("Containerfile.feature-producer"));
        assert!(r.image_name.ends_with("-feature-producer"));
    }

    #[test]
    fn project_default_used_when_no_role_specific() {
        let tmp = TempDir::new().unwrap();
        let agents = tmp.path().join(".agents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(agents.join("Containerfile"), "FROM agent-base").unwrap();

        let cfg = cfg_with(tmp.path().to_path_buf());
        let r = resolve(&cfg, "planner");

        assert_eq!(r.containerfile, agents.join("Containerfile"));
        assert!(!r.image_name.ends_with("-planner"));
    }

    #[test]
    fn falls_back_to_base_when_no_project_dotagents() {
        let tmp = TempDir::new().unwrap();
        let cfg = cfg_with(tmp.path().to_path_buf());
        let r = resolve(&cfg, "planner");

        assert_eq!(r.image_name, BASE_IMAGE);
    }

    #[test]
    fn slug_sanitizes_directory_name() {
        assert_eq!(project_slug(Path::new("/foo/My Project_1.0")), "my-project-1-0");
        assert_eq!(project_slug(Path::new("/foo/-leading-hyphen-")), "leading-hyphen");
        assert_eq!(project_slug(Path::new("/")), "project");
    }

    #[test]
    fn bundled_base_returns_containerfile_base() {
        let tmp = TempDir::new().unwrap();
        let base_path = tmp.path().join("Containerfile.base");
        fs::write(&base_path, "FROM scratch").unwrap();
        let cfg = cfg_with(tmp.path().to_path_buf());
        let result = bundled_base_containerfile(&cfg).unwrap();
        assert_eq!(result.file_name().unwrap(), "Containerfile.base");
    }
}
