//! Host-mediated integration: merge a team's work branch into the base branch
//! without a PR. This is the dogfooding gate from SEALED_TEAMS_PLAN — the host
//! is the only party with write access to the canonical repo, so it performs
//! the merge after a review pass. Agents never push here.
//!
//! `integrate` is pure orchestration over the `MergeOps` trait, so the decision
//! logic (refuse on a dirty tree, refuse if the branch is missing, emit the
//! right git verbs in order) is testable without touching a real repo.

use std::path::Path;

/// Git operations needed to inspect and integrate a work branch. Injectable so
/// `integrate` can be unit-tested against a fake.
pub trait MergeOps: Send + Sync {
    fn current_branch(&self, repo: &Path) -> Result<String, String>;
    fn branch_exists(&self, repo: &Path, branch: &str) -> bool;
    fn working_tree_clean(&self, repo: &Path) -> Result<bool, String>;
    /// `git diff --stat <base>...<branch>` (three-dot: changes on branch since
    /// it diverged from base).
    fn diff_stat(&self, repo: &Path, base: &str, branch: &str) -> Result<String, String>;
    /// Full `git diff <base>...<branch>`.
    fn diff(&self, repo: &Path, base: &str, branch: &str) -> Result<String, String>;
    /// `git merge --no-ff <branch> -m <msg>` — assumes the repo is on `base`.
    fn merge_no_ff(&self, repo: &Path, branch: &str, msg: &str) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrateMode {
    /// Show the diff for review; do not mutate the repo.
    Check,
    /// Merge the work branch into base (no fast-forward).
    Merge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrateReport {
    pub base: String,
    pub branch: String,
    pub diff_stat: String,
    /// Full diff, populated in Check mode (the review subagent reads this).
    pub diff: Option<String>,
    pub merged: bool,
}

/// What to merge and where. Built from a team manifest by the caller.
pub struct IntegrateSpec {
    pub team_id: String,
    pub ticket_id: String,
    pub base_branch: String,
    pub work_branch: String,
}

/// Inspect (Check) or merge (Merge) a team's work branch into its base.
/// Refuses to merge unless the canonical repo is on the base branch with a
/// clean working tree — merging into a dirty or unexpected branch would
/// silently entangle unrelated changes.
pub fn integrate(
    ops: &dyn MergeOps,
    repo: &Path,
    spec: &IntegrateSpec,
    mode: IntegrateMode,
) -> Result<IntegrateReport, String> {
    if !ops.branch_exists(repo, &spec.work_branch) {
        return Err(format!(
            "work branch '{}' does not exist in {}",
            spec.work_branch,
            repo.display()
        ));
    }

    let diff_stat = ops.diff_stat(repo, &spec.base_branch, &spec.work_branch)?;

    match mode {
        IntegrateMode::Check => {
            let diff = ops.diff(repo, &spec.base_branch, &spec.work_branch)?;
            Ok(IntegrateReport {
                base: spec.base_branch.clone(),
                branch: spec.work_branch.clone(),
                diff_stat,
                diff: Some(diff),
                merged: false,
            })
        }
        IntegrateMode::Merge => {
            let current = ops.current_branch(repo)?;
            if current != spec.base_branch {
                return Err(format!(
                    "canonical repo is on '{}', not base '{}'; refusing to merge",
                    current, spec.base_branch
                ));
            }
            if !ops.working_tree_clean(repo)? {
                return Err(
                    "canonical repo has a dirty working tree; commit or stash before integrating"
                        .into(),
                );
            }
            let msg = format!(
                "Integrate {team} ({ticket}): merge {branch} into {base}",
                team = spec.team_id,
                ticket = spec.ticket_id,
                branch = spec.work_branch,
                base = spec.base_branch,
            );
            ops.merge_no_ff(repo, &spec.work_branch, &msg)?;
            Ok(IntegrateReport {
                base: spec.base_branch.clone(),
                branch: spec.work_branch.clone(),
                diff_stat,
                diff: None,
                merged: true,
            })
        }
    }
}

/// Real git implementation, shelling out via `git -C <repo>`.
pub struct RealMergeOps;

impl RealMergeOps {
    fn run(repo: &Path, args: &[&str]) -> Result<std::process::Output, String> {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .map_err(|e| format!("git {}: {}", args.join(" "), e))
    }

    fn run_ok(repo: &Path, args: &[&str]) -> Result<String, String> {
        let out = Self::run(repo, args)?;
        if !out.status.success() {
            return Err(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }
}

impl MergeOps for RealMergeOps {
    fn current_branch(&self, repo: &Path) -> Result<String, String> {
        Ok(Self::run_ok(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .to_string())
    }

    fn branch_exists(&self, repo: &Path, branch: &str) -> bool {
        Self::run(
            repo,
            &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", branch)],
        )
        .map(|o| o.status.success())
        .unwrap_or(false)
    }

    fn working_tree_clean(&self, repo: &Path) -> Result<bool, String> {
        Ok(Self::run_ok(repo, &["status", "--porcelain"])?
            .trim()
            .is_empty())
    }

    fn diff_stat(&self, repo: &Path, base: &str, branch: &str) -> Result<String, String> {
        Self::run_ok(repo, &["diff", "--stat", &format!("{}...{}", base, branch)])
    }

    fn diff(&self, repo: &Path, base: &str, branch: &str) -> Result<String, String> {
        Self::run_ok(repo, &["diff", &format!("{}...{}", base, branch)])
    }

    fn merge_no_ff(&self, repo: &Path, branch: &str, msg: &str) -> Result<(), String> {
        Self::run_ok(repo, &["merge", "--no-ff", branch, "-m", msg])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;

    struct FakeGit {
        on_base: bool,
        clean: bool,
        branch_present: bool,
        calls: Mutex<Vec<String>>,
    }

    impl FakeGit {
        fn new() -> Self {
            Self {
                on_base: true,
                clean: true,
                branch_present: true,
                calls: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl MergeOps for FakeGit {
        fn current_branch(&self, _repo: &Path) -> Result<String, String> {
            Ok(if self.on_base { "main" } else { "other" }.into())
        }
        fn branch_exists(&self, _repo: &Path, _branch: &str) -> bool {
            self.branch_present
        }
        fn working_tree_clean(&self, _repo: &Path) -> Result<bool, String> {
            Ok(self.clean)
        }
        fn diff_stat(&self, _repo: &Path, _base: &str, _branch: &str) -> Result<String, String> {
            Ok(" file.rs | 2 +-".into())
        }
        fn diff(&self, _repo: &Path, _base: &str, _branch: &str) -> Result<String, String> {
            Ok("@@ diff @@".into())
        }
        fn merge_no_ff(&self, _repo: &Path, branch: &str, _msg: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("merge {}", branch));
            Ok(())
        }
    }

    fn spec() -> IntegrateSpec {
        IntegrateSpec {
            team_id: "t-x".into(),
            ticket_id: "x".into(),
            base_branch: "main".into(),
            work_branch: "t-x/code".into(),
        }
    }

    fn repo() -> PathBuf {
        PathBuf::from("/tmp/repo")
    }

    #[test]
    fn check_returns_diff_without_merging() {
        let git = FakeGit::new();
        let report = integrate(&git, &repo(), &spec(), IntegrateMode::Check).unwrap();
        assert!(!report.merged);
        assert_eq!(report.diff.as_deref(), Some("@@ diff @@"));
        assert!(git.calls().is_empty(), "check must not merge");
    }

    #[test]
    fn merge_runs_when_on_base_and_clean() {
        let git = FakeGit::new();
        let report = integrate(&git, &repo(), &spec(), IntegrateMode::Merge).unwrap();
        assert!(report.merged);
        assert_eq!(report.diff, None);
        assert_eq!(git.calls(), vec!["merge t-x/code".to_string()]);
    }

    #[test]
    fn merge_refused_when_not_on_base() {
        let mut git = FakeGit::new();
        git.on_base = false;
        let err = integrate(&git, &repo(), &spec(), IntegrateMode::Merge).unwrap_err();
        assert!(err.contains("not base"), "got: {}", err);
        assert!(git.calls().is_empty());
    }

    #[test]
    fn merge_refused_when_dirty() {
        let mut git = FakeGit::new();
        git.clean = false;
        let err = integrate(&git, &repo(), &spec(), IntegrateMode::Merge).unwrap_err();
        assert!(err.contains("dirty"), "got: {}", err);
        assert!(git.calls().is_empty());
    }

    #[test]
    fn refused_when_branch_missing() {
        let mut git = FakeGit::new();
        git.branch_present = false;
        let err = integrate(&git, &repo(), &spec(), IntegrateMode::Check).unwrap_err();
        assert!(err.contains("does not exist"), "got: {}", err);
    }
}
