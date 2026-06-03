//! Team manager: groups agents into PR-scoped teams that own one bd ticket
//! from spec to merge. Each team gets its own git clone per role, its own state
//! directory, and three agents (planner / producer / reviewer). When the PR
//! merges the team is destroyed; when the PR is awaiting humans the team
//! self-suspends and can wake later.
//!
//! This module is the lifecycle authority. Spawning/suspending/resuming a
//! team is a single TeamManager call that drives clones, manifest, and
//! container operations together. The injected traits (GitOps, ContainerOps,
//! ShellOps) make every step testable without touching git or podman.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Lifecycle state for a team. The state machine is:
/// `Spawning → Active → Suspending → Suspended ⇄ Active → Completed | Failed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TeamState {
    Spawning,
    Active,
    Suspending,
    Suspended,
    Completed,
    Failed,
}

/// Logical role for an agent within a team. Compact form keeps container and
/// tmux-window names short.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamAgent {
    pub role: String, // canonical role name (e.g. "planner")
    pub name: String, // container/tmux name (e.g. "t-bd42-plan")
}

/// On-disk manifest for a team. Persisted at `.teams/<id>/manifest.json` and
/// rewritten on every state transition. Survives orchestrator restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub ticket_id: String,
    pub base_branch: String,
    pub work_branch: String,
    /// Per-role clone paths on the host, under `.teams-clones/<team-id>/<role>/`.
    pub clones: BTreeMap<String, PathBuf>,
    pub state: TeamState,
    pub agents: Vec<TeamAgent>,
    pub pr_url: Option<String>,
    pub pr_number: Option<u64>,
    pub created_at: String,
    pub last_active: String,
    pub suspend_reason: Option<String>,
}

/// Abstraction over git clone/checkout/fetch operations. Injectable for testing.
pub trait GitOps: Send + Sync {
    /// `git clone --local <src> <dest>` — cheap hardlinked clone.
    fn clone_local(&self, src: &Path, dest: &Path) -> Result<(), String>;

    /// `git -C <repo> checkout -B <branch> <base>` — create or reset branch.
    fn checkout_new_branch(&self, repo: &Path, branch: &str, base: &str) -> Result<(), String>;

    /// `git -C <canonical_repo> fetch <src_clone> <branch>:<branch>` — pull
    /// the named branch from a role clone into the canonical repo.
    fn fetch_branch(
        &self,
        canonical_repo: &Path,
        src_clone: &Path,
        branch: &str,
    ) -> Result<(), String>;

    /// `git branch -D <branch>` — delete the team branch when team completes.
    /// Best-effort; ignored on error.
    fn branch_delete(&self, repo: &Path, branch: &str);
}

pub struct RealGitOps;

impl GitOps for RealGitOps {
    fn clone_local(&self, src: &Path, dest: &Path) -> Result<(), String> {
        let out = std::process::Command::new("git")
            .args(["clone", "--local"])
            .arg(src)
            .arg(dest)
            .output()
            .map_err(|e| format!("git clone --local: {}", e))?;
        if !out.status.success() {
            return Err(format!(
                "git clone --local failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    }

    fn checkout_new_branch(&self, repo: &Path, branch: &str, base: &str) -> Result<(), String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["checkout", "-B", branch, base])
            .output()
            .map_err(|e| format!("git checkout -B: {}", e))?;
        if !out.status.success() {
            return Err(format!(
                "git checkout -B failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    }

    fn fetch_branch(
        &self,
        canonical_repo: &Path,
        src_clone: &Path,
        branch: &str,
    ) -> Result<(), String> {
        let refspec = format!("{}:{}", branch, branch);
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(canonical_repo)
            .arg("fetch")
            .arg(src_clone)
            .arg(&refspec)
            .output()
            .map_err(|e| format!("git fetch: {}", e))?;
        if !out.status.success() {
            return Err(format!(
                "git fetch failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    }

    fn branch_delete(&self, repo: &Path, branch: &str) {
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["branch", "-D", branch])
            .output();
    }
}

/// Hit returned by TeamLookup when an agent belongs to a team.
#[derive(Debug, Clone)]
pub struct TeamLookupHit {
    pub team_id: String,
    pub work_branch: String,
}

/// Looks up which team (if any) a given agent name belongs to.
/// Injectable for testing; see `NoTeamLookup` and `ManifestDirTeamLookup`.
pub trait TeamLookup: Send + Sync {
    fn team_for_agent(&self, agent_name: &str) -> Option<TeamLookupHit>;
}

/// No-op lookup — always returns None. Default when no real lookup is wired.
pub struct NoTeamLookup;

impl TeamLookup for NoTeamLookup {
    fn team_for_agent(&self, _agent_name: &str) -> Option<TeamLookupHit> {
        None
    }
}

/// Reads `.teams/<id>/manifest.json` on every call to find which team an
/// agent belongs to. No cache: git_push is infrequent and a fresh read stays
/// correct across team state transitions.
pub struct ManifestDirTeamLookup {
    teams_dir: PathBuf,
}

impl ManifestDirTeamLookup {
    pub fn new(project_root: &Path) -> Self {
        Self {
            teams_dir: project_root.join(".teams"),
        }
    }
}

impl TeamLookup for ManifestDirTeamLookup {
    fn team_for_agent(&self, agent_name: &str) -> Option<TeamLookupHit> {
        if !self.teams_dir.exists() {
            return None;
        }
        let entries = std::fs::read_dir(&self.teams_dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("manifest.json");
            if !manifest_path.is_file() {
                continue;
            }
            if let Ok(team) = parse_manifest_file(&manifest_path) {
                if team.agents.iter().any(|a| a.name == agent_name) {
                    return Some(TeamLookupHit {
                        team_id: team.id,
                        work_branch: team.work_branch,
                    });
                }
            }
        }
        None
    }
}

/// What's needed to create a team. The CLI builds this from a bd ticket id.
pub struct SpawnSpec {
    pub ticket_id: String,
    pub base_branch: String,
    /// Roles that make up the team, in spawn order. For MVT the trio is
    /// `[("planner","plan"), ("feature-producer","prod"), ("review-agent","rev")]`.
    pub roles: Vec<(String, String)>, // (role, role_short)
}

/// Owns the on-disk teams directory and clones directory; is the single
/// authority for team lifecycle. Stateless w.r.t. ticket data — bd is still
/// the source of truth — but holds the manifest cache in memory and persists
/// to disk on every transition.
pub struct TeamManager {
    project_root: PathBuf,
    teams_dir: PathBuf,
    clones_dir: PathBuf,
    git: Box<dyn GitOps>,
    teams: HashMap<String, Team>,
}

impl TeamManager {
    pub fn new(project_root: PathBuf, git: Box<dyn GitOps>) -> Self {
        let teams_dir = project_root.join(".teams");
        let clones_dir = project_root.join(".teams-clones");
        Self {
            project_root,
            teams_dir,
            clones_dir,
            git,
            teams: HashMap::new(),
        }
    }

    /// Read all manifests from disk into memory. Call once at startup so the
    /// CLI/orchestrator picks up teams from a previous session.
    pub fn load_from_disk(&mut self) -> Result<(), String> {
        if !self.teams_dir.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(&self.teams_dir)
            .map_err(|e| format!("read .teams: {}", e))?
        {
            let entry = entry.map_err(|e| format!("read .teams entry: {}", e))?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("manifest.json");
            if !manifest_path.is_file() {
                continue;
            }
            let team = parse_manifest_file(&manifest_path)
                .map_err(|e| format!("load manifest {}: {}", manifest_path.display(), e))?;
            self.teams.insert(team.id.clone(), team);
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<&Team> {
        self.teams.values().collect()
    }

    pub fn get(&self, id: &str) -> Option<&Team> {
        self.teams.get(id)
    }

    /// Return the clone path on host for a given team's role, if known.
    pub fn clone_path(&self, team_id: &str, role: &str) -> Option<&Path> {
        self.teams.get(team_id)?.clones.get(role).map(|p| p.as_path())
    }

    /// Fetch the role's work branch from its clone into the canonical repo.
    /// Returns Err for unknown team or role.
    pub fn fetch_role_branch(&self, team_id: &str, role: &str) -> Result<(), String> {
        let team = self
            .teams
            .get(team_id)
            .ok_or_else(|| format!("team '{}' not found", team_id))?;
        let clone = team
            .clones
            .get(role)
            .ok_or_else(|| format!("no clone for role '{}' in team '{}'", role, team_id))?;
        self.git
            .fetch_branch(&self.project_root, clone, &team.work_branch)
    }

    /// Provision one clone per role, set up state directories, write the manifest.
    /// Does not start containers — the caller (CLI) does that, because spawning
    /// containers is wired through the existing run-agent flow.
    pub fn create_team(&mut self, spec: SpawnSpec) -> Result<&Team, String> {
        let id = team_id(&spec.ticket_id);
        if self.teams.contains_key(&id) {
            return Err(format!("Team '{}' already exists", id));
        }

        let work_branch = format!("{}/code", id);

        std::fs::create_dir_all(&self.clones_dir)
            .map_err(|e| format!("create clones dir: {}", e))?;

        let mut clones: BTreeMap<String, PathBuf> = BTreeMap::new();

        for (role, _short) in &spec.roles {
            let clone_path = self.clones_dir.join(&id).join(role);
            if clone_path.exists() {
                std::fs::remove_dir_all(&clone_path)
                    .map_err(|e| format!("remove stale clone {}: {}", clone_path.display(), e))?;
            }
            self.git.clone_local(&self.project_root, &clone_path)?;
            self.git
                .checkout_new_branch(&clone_path, &work_branch, &spec.base_branch)?;
            clones.insert(role.clone(), clone_path);
        }

        let agents: Vec<TeamAgent> = spec
            .roles
            .iter()
            .map(|(role, short)| TeamAgent {
                role: role.clone(),
                name: format!("{}-{}", id, short),
            })
            .collect();

        let team_dir = self.teams_dir.join(&id);
        std::fs::create_dir_all(&team_dir).map_err(|e| format!("create team dir: {}", e))?;
        for agent in &agents {
            std::fs::create_dir_all(team_dir.join(&agent.role))
                .map_err(|e| format!("create agent state dir: {}", e))?;
        }

        let now = now_iso();
        let team = Team {
            id: id.clone(),
            ticket_id: spec.ticket_id,
            base_branch: spec.base_branch,
            work_branch,
            clones,
            state: TeamState::Spawning,
            agents,
            pr_url: None,
            pr_number: None,
            created_at: now.clone(),
            last_active: now,
            suspend_reason: None,
        };

        self.write_manifest(&team)?;
        self.teams.insert(id.clone(), team);
        Ok(self.teams.get(&id).unwrap())
    }

    /// Mark a team active once its containers have all registered. Idempotent.
    pub fn mark_active(&mut self, id: &str) -> Result<(), String> {
        let team = self
            .teams
            .get_mut(id)
            .ok_or_else(|| format!("team '{}' not found", id))?;
        team.state = TeamState::Active;
        team.last_active = now_iso();
        let snapshot = team.clone();
        self.write_manifest(&snapshot)
    }

    /// Update only the state field (e.g., Suspending → Suspended after the
    /// snapshot finishes). Persists.
    pub fn set_state(
        &mut self,
        id: &str,
        state: TeamState,
        reason: Option<String>,
    ) -> Result<(), String> {
        let team = self
            .teams
            .get_mut(id)
            .ok_or_else(|| format!("team '{}' not found", id))?;
        team.state = state;
        team.last_active = now_iso();
        if reason.is_some() {
            team.suspend_reason = reason;
        }
        let snapshot = team.clone();
        self.write_manifest(&snapshot)
    }

    /// Tear down all per-role clone directories and archive the manifest. Used
    /// by both Completed (PR merged) and Failed (operator killed) transitions.
    pub fn teardown(&mut self, id: &str, archive: bool) -> Result<(), String> {
        let team = self
            .teams
            .remove(id)
            .ok_or_else(|| format!("team '{}' not found", id))?;

        for (_role, clone_path) in &team.clones {
            let _ = std::fs::remove_dir_all(clone_path);
        }

        self.git
            .branch_delete(&self.project_root, &team.work_branch);

        let team_dir = self.teams_dir.join(&team.id);
        if archive {
            let archive_dir = self.teams_dir.join("archive");
            let _ = std::fs::create_dir_all(&archive_dir);
            let target = archive_dir.join(&team.id);
            let _ = std::fs::rename(&team_dir, &target);
        } else {
            let _ = std::fs::remove_dir_all(&team_dir);
        }
        Ok(())
    }

    /// Record the open PR on the team manifest (called after gh_pr_create succeeds).
    pub fn set_pr(&mut self, team_id: &str, url: &str, number: u64) -> Result<(), String> {
        let team = self
            .teams
            .get_mut(team_id)
            .ok_or_else(|| format!("team '{}' not found", team_id))?;
        team.pr_url = Some(url.to_string());
        team.pr_number = Some(number);
        team.last_active = now_iso();
        let snapshot = team.clone();
        self.write_manifest(&snapshot)
    }

    /// Return `(team_id, ticket_id, work_branch, pr_number)` for every Active
    /// team that has an open PR number recorded. Results are sorted by team_id
    /// for deterministic ordering.
    pub fn teams_with_open_pr(&self) -> Vec<(String, String, String, u64)> {
        let mut result: Vec<_> = self
            .teams
            .values()
            .filter(|t| t.state == TeamState::Active && t.pr_number.is_some())
            .map(|t| {
                (
                    t.id.clone(),
                    t.ticket_id.clone(),
                    t.work_branch.clone(),
                    t.pr_number.unwrap(),
                )
            })
            .collect();
        result.sort_by(|a, b| a.0.cmp(&b.0));
        result
    }

    /// Per-role agent state directory; the CLI mounts this into the agent's
    /// container at /root/.claude during spawn/resume.
    pub fn agent_state_dir(&self, team_id: &str, role: &str) -> PathBuf {
        self.teams_dir.join(team_id).join(role)
    }

    /// The compacted/raw conversation snapshot path for a given role.
    pub fn conversation_snapshot_path(&self, team_id: &str, role: &str) -> PathBuf {
        self.agent_state_dir(team_id, role)
            .join("conversation.jsonl")
    }

    fn write_manifest(&self, team: &Team) -> Result<(), String> {
        let dir = self.teams_dir.join(&team.id);
        std::fs::create_dir_all(&dir).map_err(|e| format!("create team dir: {}", e))?;
        let path = dir.join("manifest.json");
        let json = serde_json::to_string_pretty(team)
            .map_err(|e| format!("serialize manifest: {}", e))?;
        std::fs::write(&path, json)
            .map_err(|e| format!("write manifest {}: {}", path.display(), e))
    }
}

/// Build a team id from a ticket id. The ticket id may contain `.` (bd's
/// child notation, e.g. `agent-in-docker-0fw.2`); we sanitize to keep
/// container/branch names safe.
pub fn team_id(ticket_id: &str) -> String {
    let mut s = String::with_capacity(ticket_id.len() + 2);
    s.push_str("t-");
    for c in ticket_id.chars() {
        if c.is_ascii_alphanumeric() || c == '-' {
            s.push(c);
        } else {
            s.push('-');
        }
    }
    s
}

fn parse_manifest_file(path: &Path) -> Result<Team, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("read {}: {}", path.display(), e))?;
    serde_json::from_str(&content)
        .map_err(|e| format!("parse {}: {}", path.display(), e))
}

fn now_iso() -> String {
    // Avoid pulling in chrono; SystemTime → seconds since epoch is enough for
    // an audit timestamp. The format is sortable and stable.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("ts:{}", secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Records git operations without executing them. Holds an Arc so callers
    /// can inspect calls after the manager takes ownership of the FakeGit.
    struct FakeGit {
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl FakeGit {
        fn new() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        /// Clone the Arc so callers can read calls after `Box::new(self)` is moved.
        fn calls_arc(&self) -> Arc<Mutex<Vec<String>>> {
            self.calls.clone()
        }
    }

    impl GitOps for FakeGit {
        fn clone_local(&self, _src: &Path, dest: &Path) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("clone_local dest={}", dest.display()));
            std::fs::create_dir_all(dest).unwrap();
            Ok(())
        }
        fn checkout_new_branch(
            &self,
            repo: &Path,
            branch: &str,
            base: &str,
        ) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!(
                "checkout_new_branch repo={} branch={} base={}",
                repo.display(),
                branch,
                base
            ));
            Ok(())
        }
        fn fetch_branch(
            &self,
            canonical_repo: &Path,
            src_clone: &Path,
            branch: &str,
        ) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!(
                "fetch_branch canonical={} src={} branch={}",
                canonical_repo.display(),
                src_clone.display(),
                branch
            ));
            Ok(())
        }
        fn branch_delete(&self, _repo: &Path, branch: &str) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("branch-delete {}", branch));
        }
    }

    fn recorded(calls: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        calls.lock().unwrap().clone()
    }

    fn mvt_roles() -> Vec<(String, String)> {
        vec![
            ("planner".into(), "plan".into()),
            ("feature-producer".into(), "prod".into()),
            ("review-agent".into(), "rev".into()),
        ]
    }

    fn write_minimal_manifest(
        dir: &std::path::Path,
        team_id: &str,
        work_branch: &str,
        agent_name: &str,
    ) {
        let team_dir = dir.join(".teams").join(team_id);
        std::fs::create_dir_all(&team_dir).unwrap();
        let manifest = serde_json::json!({
            "id": team_id,
            "ticket_id": "ticket-1",
            "base_branch": "main",
            "work_branch": work_branch,
            "clones": {"feature-producer": "/tmp/fake-clone"},
            "state": "active",
            "agents": [{"role": "feature-producer", "name": agent_name}],
            "pr_url": null,
            "pr_number": null,
            "created_at": "ts:0",
            "last_active": "ts:0",
            "suspend_reason": null
        });
        std::fs::write(
            team_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    // ── new spec tests ────────────────────────────────────────────────────────

    #[test]
    fn create_team_provisions_one_clone_per_role() {
        let tmp = tempfile::tempdir().unwrap();
        let git = FakeGit::new();
        let calls = git.calls_arc();
        let mut mgr = TeamManager::new(tmp.path().into(), Box::new(git));

        let team = mgr
            .create_team(SpawnSpec {
                ticket_id: "abc".into(),
                base_branch: "main".into(),
                roles: mvt_roles(),
            })
            .unwrap()
            .clone();

        let all_calls = recorded(&calls);
        let clone_calls: Vec<_> = all_calls
            .iter()
            .filter(|c| c.starts_with("clone_local"))
            .collect();
        let checkout_calls: Vec<_> = all_calls
            .iter()
            .filter(|c| c.starts_with("checkout_new_branch"))
            .collect();

        // Exactly 3 clone_local + 3 checkout_new_branch, one per role
        assert_eq!(clone_calls.len(), 3, "must have 3 clone_local calls, got: {:?}", all_calls);
        assert_eq!(
            checkout_calls.len(),
            3,
            "must have 3 checkout_new_branch calls, got: {:?}",
            all_calls
        );

        // Each clone_local dest is under .teams-clones/<id>/<role>
        let base = tmp.path().join(".teams-clones").join(&team.id);
        for role in &["planner", "feature-producer", "review-agent"] {
            let expected = base.join(role).display().to_string();
            assert!(
                clone_calls.iter().any(|c| c.contains(&expected)),
                "missing clone_local for role {} (expected dest {})",
                role,
                expected
            );
        }

        // No worktree_add calls
        assert!(
            !all_calls.iter().any(|c| c.starts_with("add ")),
            "no worktree_add expected; got: {:?}",
            all_calls
        );

        // All checkout_new_branch calls use the work branch off main
        for c in &checkout_calls {
            assert!(
                c.contains(&format!("branch={}", team.work_branch)),
                "checkout must use work_branch; got: {}",
                c
            );
            assert!(c.contains("base=main"), "checkout must use base=main; got: {}", c);
        }
    }

    #[test]
    fn create_team_writes_clone_paths_into_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));
        let team = mgr
            .create_team(SpawnSpec {
                ticket_id: "manifest-test".into(),
                base_branch: "main".into(),
                roles: mvt_roles(),
            })
            .unwrap()
            .clone();

        // Reload from disk to verify manifest persisted the clones map
        let mut mgr2 = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));
        mgr2.load_from_disk().unwrap();
        let reloaded = mgr2.get(&team.id).unwrap();

        assert_eq!(reloaded.clones.len(), 3, "clones map must have one entry per role");
        for role in &["planner", "feature-producer", "review-agent"] {
            assert!(
                reloaded.clones.contains_key(*role),
                "clones map must contain role {}",
                role
            );
        }

        // clone_path accessor returns each entry
        for role in &["planner", "feature-producer", "review-agent"] {
            let via_accessor = mgr2.clone_path(&team.id, role);
            assert!(via_accessor.is_some(), "clone_path must return Some for {}", role);
            assert_eq!(
                via_accessor.unwrap(),
                reloaded.clones[*role].as_path(),
                "clone_path must match clones map for {}",
                role
            );
        }
    }

    #[test]
    fn create_team_cleans_stale_clone_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));

        // Pre-create the planner clone dir with a sentinel file
        let id_str = team_id("stale-test");
        let stale_dir = tmp
            .path()
            .join(".teams-clones")
            .join(&id_str)
            .join("planner");
        std::fs::create_dir_all(&stale_dir).unwrap();
        let sentinel = stale_dir.join("SENTINEL");
        std::fs::write(&sentinel, "stale").unwrap();
        assert!(sentinel.exists(), "sentinel must exist before create_team");

        mgr.create_team(SpawnSpec {
            ticket_id: "stale-test".into(),
            base_branch: "main".into(),
            roles: mvt_roles(),
        })
        .unwrap();

        assert!(
            !sentinel.exists(),
            "sentinel must be removed before re-cloning"
        );
    }

    #[test]
    fn teardown_removes_every_clone_dir_and_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let git = FakeGit::new();
        let calls = git.calls_arc();
        let mut mgr = TeamManager::new(tmp.path().into(), Box::new(git));

        let team = mgr
            .create_team(SpawnSpec {
                ticket_id: "teardown-test".into(),
                base_branch: "main".into(),
                roles: mvt_roles(),
            })
            .unwrap()
            .clone();

        // Verify clone dirs exist after creation (FakeGit::clone_local creates them)
        for (_role, path) in &team.clones {
            assert!(path.exists(), "clone dir must exist before teardown: {}", path.display());
        }

        let clone_paths: Vec<PathBuf> = team.clones.values().cloned().collect();
        let work_branch = team.work_branch.clone();

        // Reset call log before teardown so we isolate teardown calls
        calls.lock().unwrap().clear();

        mgr.teardown(&team.id, false).unwrap();

        // Every clone dir is gone
        for path in &clone_paths {
            assert!(
                !path.exists(),
                "clone dir must be removed after teardown: {}",
                path.display()
            );
        }

        // Exactly one branch_delete for the work branch
        let all_calls = recorded(&calls);
        let branch_deletes: Vec<_> = all_calls
            .iter()
            .filter(|c| c.starts_with("branch-delete"))
            .collect();
        assert_eq!(
            branch_deletes.len(),
            1,
            "must have exactly one branch_delete; got: {:?}",
            all_calls
        );
        assert!(
            branch_deletes[0].contains(&work_branch),
            "branch_delete must name work branch {}; got: {}",
            work_branch,
            branch_deletes[0]
        );
    }

    #[test]
    fn fetch_role_branch_uses_fixed_refspec() {
        let tmp = tempfile::tempdir().unwrap();
        let git = FakeGit::new();
        let calls = git.calls_arc();
        let mut mgr = TeamManager::new(tmp.path().into(), Box::new(git));

        let team = mgr
            .create_team(SpawnSpec {
                ticket_id: "fetch-test".into(),
                base_branch: "main".into(),
                roles: mvt_roles(),
            })
            .unwrap()
            .clone();

        // Clear create_team calls
        calls.lock().unwrap().clear();

        mgr.fetch_role_branch(&team.id, "planner").unwrap();

        let all_calls = recorded(&calls);
        let fetch_calls: Vec<_> = all_calls
            .iter()
            .filter(|c| c.starts_with("fetch_branch"))
            .collect();
        assert_eq!(fetch_calls.len(), 1, "must record exactly one fetch_branch call");

        let expected_canonical = tmp.path().display().to_string();
        let expected_src = team.clones["planner"].display().to_string();

        assert!(
            fetch_calls[0].contains(&format!("canonical={}", expected_canonical)),
            "fetch_branch must use project_root as canonical; got: {}",
            fetch_calls[0]
        );
        assert!(
            fetch_calls[0].contains(&format!("src={}", expected_src)),
            "fetch_branch must use planner clone as src; got: {}",
            fetch_calls[0]
        );
        assert!(
            fetch_calls[0].contains(&format!("branch={}", team.work_branch)),
            "fetch_branch must use work_branch; got: {}",
            fetch_calls[0]
        );

        // Unknown team returns Err
        assert!(
            mgr.fetch_role_branch("no-such-team", "planner").is_err(),
            "unknown team must return Err"
        );
        // Unknown role returns Err
        assert!(
            mgr.fetch_role_branch(&team.id, "no-such-role").is_err(),
            "unknown role must return Err"
        );
    }

    #[test]
    fn load_from_disk_round_trips_clones_map() {
        let tmp = tempfile::tempdir().unwrap();
        let (team_id_str, clones_snapshot) = {
            let mut mgr = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));
            let team = mgr
                .create_team(SpawnSpec {
                    ticket_id: "rt-test".into(),
                    base_branch: "main".into(),
                    roles: mvt_roles(),
                })
                .unwrap()
                .clone();
            (team.id.clone(), team.clones.clone())
        };
        let mut mgr2 = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));
        mgr2.load_from_disk().unwrap();
        let reloaded = mgr2.get(&team_id_str).unwrap();
        assert_eq!(
            reloaded.clones, clones_snapshot,
            "clones map must survive disk round-trip"
        );
    }

    // ── existing tests (updated for new manifest schema) ─────────────────────

    #[test]
    fn manifest_dir_lookup_finds_agent() {
        let tmp = tempfile::tempdir().unwrap();
        write_minimal_manifest(tmp.path(), "foo", "feat/x", "X");
        let lookup = ManifestDirTeamLookup::new(tmp.path());
        let hit = lookup.team_for_agent("X").expect("X must be found");
        assert_eq!(hit.team_id, "foo");
        assert_eq!(hit.work_branch, "feat/x");
        assert!(lookup.team_for_agent("Y").is_none());
    }

    #[test]
    fn manifest_dir_lookup_fresh_read_across_calls() {
        let tmp = tempfile::tempdir().unwrap();
        write_minimal_manifest(tmp.path(), "foo", "feat/x", "X");
        let lookup = ManifestDirTeamLookup::new(tmp.path());
        assert!(lookup.team_for_agent("Y").is_none());
        // Add a second manifest and confirm it's visible on the next call.
        write_minimal_manifest(tmp.path(), "bar", "feat/y", "Y");
        let hit = lookup
            .team_for_agent("Y")
            .expect("Y must be found after second manifest");
        assert_eq!(hit.team_id, "bar");
        assert_eq!(hit.work_branch, "feat/y");
    }

    #[test]
    fn manifest_dir_lookup_missing_dir_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let lookup = ManifestDirTeamLookup::new(tmp.path());
        assert!(lookup.team_for_agent("X").is_none());
    }

    #[test]
    fn team_id_sanitizes_dots() {
        assert_eq!(team_id("agent-in-docker-0fw.2"), "t-agent-in-docker-0fw-2");
        assert_eq!(team_id("simple-1"), "t-simple-1");
    }

    #[test]
    fn create_team_provisions_clones_manifest_and_state_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));

        let team = mgr
            .create_team(SpawnSpec {
                ticket_id: "agent-in-docker-0fw.2".into(),
                base_branch: "main".into(),
                roles: mvt_roles(),
            })
            .unwrap()
            .clone();

        assert_eq!(team.id, "t-agent-in-docker-0fw-2");
        assert_eq!(team.state, TeamState::Spawning);
        assert_eq!(team.work_branch, "t-agent-in-docker-0fw-2/code");
        assert_eq!(team.agents.len(), 3);
        assert_eq!(team.clones.len(), 3, "must have one clone per role");

        // Manifest written.
        let manifest_path = tmp
            .path()
            .join(".teams")
            .join(&team.id)
            .join("manifest.json");
        assert!(manifest_path.is_file(), "manifest must be written");

        // Per-role state dirs exist.
        for agent in &team.agents {
            assert!(tmp
                .path()
                .join(".teams")
                .join(&team.id)
                .join(&agent.role)
                .is_dir());
        }

        // Clone dirs were provisioned (FakeGit creates them).
        for (_role, clone_path) in &team.clones {
            assert!(clone_path.is_dir(), "clone dir must exist: {}", clone_path.display());
        }
    }

    #[test]
    fn create_team_rejects_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));
        mgr.create_team(SpawnSpec {
            ticket_id: "abc".into(),
            base_branch: "main".into(),
            roles: mvt_roles(),
        })
        .unwrap();
        let err = mgr
            .create_team(SpawnSpec {
                ticket_id: "abc".into(),
                base_branch: "main".into(),
                roles: mvt_roles(),
            })
            .unwrap_err();
        assert!(err.contains("already exists"), "got: {}", err);
    }

    #[test]
    fn load_from_disk_reads_existing_manifests() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let mut mgr = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));
            mgr.create_team(SpawnSpec {
                ticket_id: "one".into(),
                base_branch: "main".into(),
                roles: mvt_roles(),
            })
            .unwrap();
            mgr.create_team(SpawnSpec {
                ticket_id: "two".into(),
                base_branch: "main".into(),
                roles: mvt_roles(),
            })
            .unwrap();
        }
        let mut mgr2 = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));
        mgr2.load_from_disk().unwrap();
        let ids: Vec<String> = mgr2.list().iter().map(|t| t.id.clone()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().any(|id| id == "t-one"));
        assert!(ids.iter().any(|id| id == "t-two"));
    }

    #[test]
    fn set_state_persists_across_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let id = {
            let mut mgr = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));
            let team = mgr
                .create_team(SpawnSpec {
                    ticket_id: "x".into(),
                    base_branch: "main".into(),
                    roles: mvt_roles(),
                })
                .unwrap()
                .clone();
            mgr.set_state(&team.id, TeamState::Suspended, Some("test".into()))
                .unwrap();
            team.id
        };
        let mut mgr2 = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));
        mgr2.load_from_disk().unwrap();
        let team = mgr2.get(&id).unwrap();
        assert_eq!(team.state, TeamState::Suspended);
        assert_eq!(team.suspend_reason.as_deref(), Some("test"));
    }

    #[test]
    fn set_pr_persists_to_manifest_and_survives_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let team_id_str = {
            let mut mgr = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));
            let team = mgr
                .create_team(SpawnSpec {
                    ticket_id: "pr-test".into(),
                    base_branch: "main".into(),
                    roles: mvt_roles(),
                })
                .unwrap()
                .clone();
            mgr.mark_active(&team.id).unwrap();
            mgr.set_pr(&team.id, "https://github.com/o/r/pull/7", 7)
                .unwrap();
            assert_eq!(mgr.get(&team.id).unwrap().pr_number, Some(7));
            assert_eq!(
                mgr.get(&team.id).unwrap().pr_url.as_deref(),
                Some("https://github.com/o/r/pull/7")
            );
            team.id
        };
        let mut mgr2 = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));
        mgr2.load_from_disk().unwrap();
        let team = mgr2.get(&team_id_str).unwrap();
        assert_eq!(team.pr_number, Some(7));
        assert_eq!(
            team.pr_url.as_deref(),
            Some("https://github.com/o/r/pull/7")
        );
    }

    #[test]
    fn teams_with_open_pr_returns_active_teams_with_pr_number() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));

        let t1 = mgr
            .create_team(SpawnSpec {
                ticket_id: "t1".into(),
                base_branch: "main".into(),
                roles: mvt_roles(),
            })
            .unwrap()
            .clone();
        mgr.mark_active(&t1.id).unwrap();
        mgr.set_pr(&t1.id, "https://github.com/o/r/pull/1", 1)
            .unwrap();

        let t2 = mgr
            .create_team(SpawnSpec {
                ticket_id: "t2".into(),
                base_branch: "main".into(),
                roles: mvt_roles(),
            })
            .unwrap()
            .clone();
        mgr.mark_active(&t2.id).unwrap();

        let t3 = mgr
            .create_team(SpawnSpec {
                ticket_id: "t3".into(),
                base_branch: "main".into(),
                roles: mvt_roles(),
            })
            .unwrap()
            .clone();
        mgr.mark_active(&t3.id).unwrap();
        mgr.set_pr(&t3.id, "https://github.com/o/r/pull/3", 3)
            .unwrap();
        mgr.set_state(&t3.id, TeamState::Suspended, None).unwrap();

        let result = mgr.teams_with_open_pr();
        assert_eq!(result.len(), 1, "only active team with pr_number should appear");
        let (tid, ticket, _branch, num) = &result[0];
        assert_eq!(tid, &t1.id);
        assert_eq!(ticket, "t1");
        assert_eq!(*num, 1);
    }

    #[test]
    fn teams_with_open_pr_ordering_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));

        for ticket in ["zzz", "aaa", "mmm"] {
            let t = mgr
                .create_team(SpawnSpec {
                    ticket_id: ticket.into(),
                    base_branch: "main".into(),
                    roles: mvt_roles(),
                })
                .unwrap()
                .clone();
            mgr.mark_active(&t.id).unwrap();
            mgr.set_pr(&t.id, "https://github.com/o/r/pull/1", 1)
                .unwrap();
        }

        let r1 = mgr.teams_with_open_pr();
        let r2 = mgr.teams_with_open_pr();
        let ids1: Vec<_> = r1.iter().map(|(id, _, _, _)| id.clone()).collect();
        let ids2: Vec<_> = r2.iter().map(|(id, _, _, _)| id.clone()).collect();
        assert_eq!(ids1, ids2, "ordering must be stable across calls");
        assert!(ids1.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn teardown_removes_clones_and_team_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let id;
        {
            let mut mgr = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));
            let team = mgr
                .create_team(SpawnSpec {
                    ticket_id: "x".into(),
                    base_branch: "main".into(),
                    roles: mvt_roles(),
                })
                .unwrap()
                .clone();
            id = team.id.clone();
            mgr.teardown(&team.id, false).unwrap();
        }
        let mut mgr2 = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));
        mgr2.load_from_disk().unwrap();
        assert!(mgr2.get(&id).is_none(), "team must be gone after teardown");
    }
}
