//! Team manager: groups agents into PR-scoped teams that own one bd ticket
//! from spec to merge. Each team gets its own git worktree, its own state
//! directory, and three agents (planner / producer / reviewer). When the PR
//! merges the team is destroyed; when the PR is awaiting humans the team
//! self-suspends and can wake later.
//!
//! This module is the lifecycle authority. Spawning/suspending/resuming a
//! team is a single TeamManager call that drives worktree, manifest, and
//! container operations together. The injected traits (GitOps, ContainerOps,
//! ShellOps) make every step testable without touching git or podman.

use std::collections::HashMap;
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
    pub worktree_path: PathBuf,
    pub state: TeamState,
    pub agents: Vec<TeamAgent>,
    pub pr_url: Option<String>,
    pub pr_number: Option<u64>,
    pub created_at: String,
    pub last_active: String,
    pub suspend_reason: Option<String>,
}

/// Abstraction over git worktree operations. Injectable for testing.
pub trait GitOps: Send + Sync {
    /// `git worktree add <path> -b <branch> <base>`.
    fn worktree_add(
        &self,
        repo: &Path,
        worktree_path: &Path,
        branch: &str,
        base: &str,
    ) -> Result<(), String>;

    /// `git worktree remove <path>` (use force=true to discard uncommitted).
    fn worktree_remove(&self, repo: &Path, worktree_path: &Path, force: bool)
        -> Result<(), String>;

    /// `git worktree prune` — clean up stale worktree references.
    fn worktree_prune(&self, repo: &Path) -> Result<(), String>;

    /// `git branch -D <branch>` — delete the team branch when team completes
    /// (after merge, the branch is redundant). Best-effort; ignored on error.
    fn branch_delete(&self, repo: &Path, branch: &str);
}

pub struct RealGitOps;

impl GitOps for RealGitOps {
    fn worktree_add(
        &self,
        repo: &Path,
        worktree_path: &Path,
        branch: &str,
        base: &str,
    ) -> Result<(), String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["worktree", "add"])
            .arg(worktree_path)
            .arg("-b")
            .arg(branch)
            .arg(base)
            .output()
            .map_err(|e| format!("git worktree add: {}", e))?;
        if !out.status.success() {
            return Err(format!(
                "git worktree add failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    }

    fn worktree_remove(
        &self,
        repo: &Path,
        worktree_path: &Path,
        force: bool,
    ) -> Result<(), String> {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(repo).args(["worktree", "remove"]);
        if force {
            cmd.arg("--force");
        }
        cmd.arg(worktree_path);
        let out = cmd
            .output()
            .map_err(|e| format!("git worktree remove: {}", e))?;
        if !out.status.success() {
            return Err(format!(
                "git worktree remove failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    }

    fn worktree_prune(&self, repo: &Path) -> Result<(), String> {
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["worktree", "prune"])
            .output();
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

/// Owns the on-disk teams directory and worktree directory; is the single
/// authority for team lifecycle. Stateless w.r.t. ticket data — bd is still
/// the source of truth — but holds the manifest cache in memory and persists
/// to disk on every transition.
pub struct TeamManager {
    project_root: PathBuf,
    teams_dir: PathBuf,
    worktrees_dir: PathBuf,
    git: Box<dyn GitOps>,
    teams: HashMap<String, Team>,
}

impl TeamManager {
    pub fn new(project_root: PathBuf, git: Box<dyn GitOps>) -> Self {
        let teams_dir = project_root.join(".teams");
        let worktrees_dir = project_root.join(".teams-worktrees");
        Self {
            project_root,
            teams_dir,
            worktrees_dir,
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

    /// Provision the worktree, set up state directories, write the manifest.
    /// Does not start containers — the caller (CLI) does that, because spawning
    /// containers is wired through the existing run-agent flow.
    pub fn create_team(&mut self, spec: SpawnSpec) -> Result<&Team, String> {
        let id = team_id(&spec.ticket_id);
        if self.teams.contains_key(&id) {
            return Err(format!("Team '{}' already exists", id));
        }

        let work_branch = format!("{}/code", id);
        let worktree_path = self.worktrees_dir.join(&id);

        std::fs::create_dir_all(&self.worktrees_dir)
            .map_err(|e| format!("create worktrees dir: {}", e))?;
        if worktree_path.exists() {
            // Stale worktree from a prior failed spawn. Prune and remove.
            let _ = self.git.worktree_prune(&self.project_root);
            let _ = self
                .git
                .worktree_remove(&self.project_root, &worktree_path, true);
        }
        self.git
            .worktree_add(&self.project_root, &worktree_path, &work_branch, &spec.base_branch)?;

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
            worktree_path,
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

    /// Tear down the worktree and archive the manifest. Used by both Completed
    /// (PR merged) and Failed (operator killed) transitions.
    pub fn teardown(&mut self, id: &str, archive: bool) -> Result<(), String> {
        let team = self
            .teams
            .remove(id)
            .ok_or_else(|| format!("team '{}' not found", id))?;
        let _ = self
            .git
            .worktree_remove(&self.project_root, &team.worktree_path, true);
        let _ = self.git.worktree_prune(&self.project_root);
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
        std::fs::write(&path, json).map_err(|e| format!("write manifest {}: {}", path.display(), e))
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
    use std::sync::Mutex;

    /// Records git operations without executing them. Lets us assert that
    /// TeamManager called the right git verbs in the right order.
    struct FakeGit {
        calls: Mutex<Vec<String>>,
    }

    impl FakeGit {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl GitOps for FakeGit {
        fn worktree_add(
            &self,
            _repo: &Path,
            worktree_path: &Path,
            branch: &str,
            base: &str,
        ) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!(
                "add {} -b {} {}",
                worktree_path.display(),
                branch,
                base
            ));
            // Pretend the worktree directory now exists so subsequent existence
            // checks behave like the real implementation.
            std::fs::create_dir_all(worktree_path).unwrap();
            Ok(())
        }
        fn worktree_remove(
            &self,
            _repo: &Path,
            worktree_path: &Path,
            force: bool,
        ) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("remove {} force={}", worktree_path.display(), force));
            let _ = std::fs::remove_dir_all(worktree_path);
            Ok(())
        }
        fn worktree_prune(&self, _repo: &Path) -> Result<(), String> {
            self.calls.lock().unwrap().push("prune".into());
            Ok(())
        }
        fn branch_delete(&self, _repo: &Path, branch: &str) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("branch-delete {}", branch));
        }
    }

    fn mvt_roles() -> Vec<(String, String)> {
        vec![
            ("planner".into(), "plan".into()),
            ("feature-producer".into(), "prod".into()),
            ("review-agent".into(), "rev".into()),
        ]
    }

    fn write_minimal_manifest(dir: &std::path::Path, team_id: &str, work_branch: &str, agent_name: &str) {
        let team_dir = dir.join(".teams").join(team_id);
        std::fs::create_dir_all(&team_dir).unwrap();
        let manifest = serde_json::json!({
            "id": team_id,
            "ticket_id": "ticket-1",
            "base_branch": "main",
            "work_branch": work_branch,
            "worktree_path": "/tmp/fake",
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
        let hit = lookup.team_for_agent("Y").expect("Y must be found after second manifest");
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
    fn create_team_provisions_worktree_manifest_and_state_dirs() {
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

        // Worktree path was provisioned.
        assert!(team.worktree_path.is_dir());
    }

    #[test]
    fn create_team_rejects_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));
        let spec = SpawnSpec {
            ticket_id: "abc".into(),
            base_branch: "main".into(),
            roles: mvt_roles(),
        };
        mgr.create_team(spec).unwrap();
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
        // Create two teams, then build a fresh manager and load.
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
        let team_id = {
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
            mgr.set_pr(&team.id, "https://github.com/o/r/pull/7", 7).unwrap();
            assert_eq!(mgr.get(&team.id).unwrap().pr_number, Some(7));
            assert_eq!(
                mgr.get(&team.id).unwrap().pr_url.as_deref(),
                Some("https://github.com/o/r/pull/7")
            );
            team.id
        };
        let mut mgr2 = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));
        mgr2.load_from_disk().unwrap();
        let team = mgr2.get(&team_id).unwrap();
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

        // Active team with PR
        let t1 = mgr
            .create_team(SpawnSpec {
                ticket_id: "t1".into(),
                base_branch: "main".into(),
                roles: mvt_roles(),
            })
            .unwrap()
            .clone();
        mgr.mark_active(&t1.id).unwrap();
        mgr.set_pr(&t1.id, "https://github.com/o/r/pull/1", 1).unwrap();

        // Active team without PR
        let t2 = mgr
            .create_team(SpawnSpec {
                ticket_id: "t2".into(),
                base_branch: "main".into(),
                roles: mvt_roles(),
            })
            .unwrap()
            .clone();
        mgr.mark_active(&t2.id).unwrap();

        // Suspended team with PR — must NOT appear
        let t3 = mgr
            .create_team(SpawnSpec {
                ticket_id: "t3".into(),
                base_branch: "main".into(),
                roles: mvt_roles(),
            })
            .unwrap()
            .clone();
        mgr.mark_active(&t3.id).unwrap();
        mgr.set_pr(&t3.id, "https://github.com/o/r/pull/3", 3).unwrap();
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
            mgr.set_pr(&t.id, "https://github.com/o/r/pull/1", 1).unwrap();
        }

        let r1 = mgr.teams_with_open_pr();
        let r2 = mgr.teams_with_open_pr();
        let ids1: Vec<_> = r1.iter().map(|(id, _, _, _)| id.clone()).collect();
        let ids2: Vec<_> = r2.iter().map(|(id, _, _, _)| id.clone()).collect();
        assert_eq!(ids1, ids2, "ordering must be stable across calls");
        // must be sorted by team_id
        assert!(ids1.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn teardown_removes_worktree_and_team_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let git = FakeGit::new();
        let calls_before;
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
            calls_before = git.calls();
            mgr.teardown(&team.id, false).unwrap();
        }
        // Reload — the team should be gone.
        let mut mgr2 = TeamManager::new(tmp.path().into(), Box::new(FakeGit::new()));
        mgr2.load_from_disk().unwrap();
        assert!(mgr2.get(&id).is_none(), "team must be gone after teardown");
        let _ = calls_before;
    }
}
