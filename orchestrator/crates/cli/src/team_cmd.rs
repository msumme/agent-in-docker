//! `agent team {spawn,suspend,resume,list,status,kill}` subcommand.
//!
//! Teams are provisioned via TeamManager (one shared git clone per team,
//! manifest, state dirs) and then 3 containers are launched — all pointed at
//! the same clone (so the reviewer sees the producer's commits). Each role
//! gets its own state directory within the shared clone. Suspend and resume
//! are container lifecycle operations over the same mounts — the per-agent
//! state is already persisted on disk by virtue of being mounted from the
//! host, so suspend = `podman rm -f` + manifest state update, and resume =
//! relaunch with the same mounts.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use orchestrator_core::integration::{self, IntegrateMode, IntegrateSpec, RealMergeOps};
use orchestrator_core::project_config;
use orchestrator_core::team_manager::{
    role_resume_policy, RealGitOps, ResumePolicy, SpawnSpec, TeamManager, TeamState,
};
use orchestrator_core::types::StartAgentPayload;

use crate::config::Config;
use crate::container;
use crate::image_resolver;
use crate::services;

/// Role-specific spawn primer. The team pipeline is sequential — planner first,
/// then producer once the spec exists, then reviewer once the PR opens. Without
/// role-aware primers each agent eagerly starts working in parallel, which
/// burns tokens and overlaps responsibilities. Producer and reviewer get
/// "wait" primers; only the planner is told to act immediately.
fn build_initial_prompt(team_id: &str, ticket_id: &str, role: &str) -> String {
    match role {
        "planner" => format!(
            "You are the PLANNER on team {team} for bd ticket {ticket}. The team \
             has just spawned and you run first.\n\n\
             1. Read your role prompt and the meta-prompt (already appended as \
                your system prompt).\n\
             2. Run `bd show {ticket}` to understand the work.\n\
             3. File the spec as a `decision` ticket parented to {ticket} per \
                your role's contract: APPROACH / FILES TO TOUCH / TEST PLAN / \
                NON-GOALS / OPEN QUESTIONS.\n\
             4. Notify the producer with `message_agent {team}-prod \"spec ready: \
                <spec-id>\"` and then stop. Your work is done until the \
                producer or reviewer kicks back with `redesign-needed`.",
            team = team_id,
            ticket = ticket_id,
        ),
        "feature-producer" | "maintenance-producer" => format!(
            "You are the PRODUCER on team {team} for bd ticket {ticket}. The team \
             has just spawned. The PLANNER runs first and will file a `decision` \
             spec ticket parented to {ticket}, then ping you.\n\n\
             Do NOT start work yet. The spec is not ready. Acknowledge with \
             one short line (e.g. \"producer ready, waiting on spec\") and stop. \
             You will be pinged via message_agent when the spec is filed.\n\n\
             When you finish your work: commit your changes in your sandbox clone, \
             then `message_agent {team}-rev \"ready for review: <sha>\"` and stop. \
             The host integrates — do not push or open a PR.\n\n\
             Available host-bridge MCP tools (call them by name): \
             `read_host_file`, `list_agents`, `message_agent`. \
             If a tool errors, report the verbatim error — do NOT conclude the \
             tool is unavailable.",
            team = team_id,
            ticket = ticket_id,
        ),
        "review-agent" => format!(
            "You are the REVIEWER on team {team} for bd ticket {ticket}. The team \
             has just spawned. The pipeline is planner → producer → you.\n\n\
             Do NOT start reviewing yet. Acknowledge with one short line (e.g. \
             \"reviewer ready, waiting on producer\") and stop. You will be pinged \
             via message_agent when the producer is ready.\n\n\
             When you review: read the producer's commits, file findings as beads \
             tickets, and on approval respond with \
             `message_agent {team}-prod \"approved: <sha>\"`. \
             There is no PR step for the agent — the host integrates.",
            team = team_id,
            ticket = ticket_id,
        ),
        other => format!(
            "You are the {role} on team {team} for bd ticket {ticket}. Read your \
             role prompt and act per your role.",
            role = other,
            team = team_id,
            ticket = ticket_id,
        ),
    }
}

/// Per-role model and effort. Planner and reviewer think; producer writes.
/// Opus 4.7 medium is the right tradeoff for the thinking roles. Sonnet 4.6
/// at high effort is cheaper and faster for the implementation pass — and
/// "high" gives the producer enough headroom to do real work without burning
/// xhigh-level tokens on every tool call.
fn role_model_effort(role: &str) -> (Option<String>, Option<String>) {
    match role {
        "feature-producer" | "maintenance-producer" => (
            Some("claude-sonnet-4-6".to_string()),
            Some("high".to_string()),
        ),
        "planner" | "review-agent" => (
            Some("claude-opus-4-7".to_string()),
            Some("medium".to_string()),
        ),
        _ => (None, None),
    }
}

/// MVT trio: planner → producer → reviewer. `feature-producer` is the default
/// producer flavor; --maintenance flips to maintenance-producer.
fn mvt_roles(maintenance: bool) -> Vec<(String, String)> {
    let producer_role = if maintenance {
        "maintenance-producer"
    } else {
        "feature-producer"
    };
    vec![
        ("planner".into(), "plan".into()),
        (producer_role.into(), "prod".into()),
        ("review-agent".into(), "rev".into()),
    ]
}

fn open_manager(cfg: &Config) -> Result<TeamManager> {
    let mut mgr = TeamManager::new(cfg.project_root.clone(), Box::new(RealGitOps));
    mgr.load_from_disk()
        .map_err(|e| anyhow::anyhow!("load teams: {}", e))?;
    Ok(mgr)
}

pub fn cmd_list(cfg: &Config) -> Result<()> {
    let mgr = open_manager(cfg)?;
    let teams = mgr.list();
    if teams.is_empty() {
        println!("No teams.");
        return Ok(());
    }
    println!(
        "{:<32} {:<10} {:<28} {}",
        "TEAM", "STATE", "TICKET", "CLONE"
    );
    for t in teams {
        println!(
            "{:<32} {:<10} {:<28} {}",
            t.id,
            format!("{:?}", t.state).to_lowercase(),
            t.ticket_id,
            t.clone_path.display()
        );
    }
    Ok(())
}

pub fn cmd_status(cfg: &Config, team_id: &str) -> Result<()> {
    let mgr = open_manager(cfg)?;
    let team = mgr
        .get(team_id)
        .ok_or_else(|| anyhow::anyhow!("team '{}' not found", team_id))?;
    println!("id:           {}", team.id);
    println!("ticket:       {}", team.ticket_id);
    println!("state:        {:?}", team.state);
    println!("base branch:  {}", team.base_branch);
    println!("work branch:  {}", team.work_branch);
    println!("created:      {}", team.created_at);
    println!("last active:  {}", team.last_active);
    if let Some(reason) = &team.suspend_reason {
        println!("suspend why:  {}", reason);
    }
    if let Some(url) = &team.pr_url {
        println!("pr:           {}", url);
    }
    println!("clone:        {}", team.clone_path.display());
    println!("agents:");
    for a in &team.agents {
        println!("  {:<6} {}", a.role, a.name);
    }
    Ok(())
}

/// Build the StartAgentPayload for one team agent — pointed at the role's own
/// clone, with a team-scoped agent_dir so each role's state stays under
/// `.teams/<team-id>/<role>/.claude/`. Resolves the role prompt with the
/// meta-prompt prepended, just like the regular Run flow.
///
/// Each agent gets its own isolated clone as project_path; no mirror-mount of
/// the canonical repo is needed because the clone is a self-contained git repo.
/// Compute whether `resume_session` should be set. Producers get ResumeContext
/// on resume; everything else always gets false (fresh start or spawn).
fn compute_resume_session(role: &str, is_resume: bool) -> bool {
    is_resume && role_resume_policy(role) == ResumePolicy::ResumeContext
}

/// Resume primer — role-aware. Producers keep their prior conversation;
/// planners and reviewers restart from scratch reading the current bd/git state.
fn build_resume_prompt(team_id: &str, ticket_id: &str, role: &str) -> String {
    match role_resume_policy(role) {
        ResumePolicy::ResumeContext => format!(
            "You are resumed with your prior context intact. Check `bd show {ticket}` \
             for any new feedback and continue.",
            ticket = ticket_id,
        ),
        ResumePolicy::FreshContext => format!(
            "You are the {role} on team {team} resuming for bd ticket {ticket}. Your \
             prior conversation is not retained. Read `bd show {ticket}` to get current \
             status, check `git log --oneline -10` for recent commits on the work branch, \
             and continue from where the team is now.",
            role = role,
            team = team_id,
            ticket = ticket_id,
        ),
    }
}

fn build_payload_for_team_agent(
    cfg: &Config,
    clone_path: &Path,
    agent_role: &str,
    agent_name: &str,
    agent_dir: PathBuf,
    role_memory_dir: PathBuf,
    dolt_port: Option<u16>,
    initial_prompt: String,
    resume_session: bool,
) -> Result<StartAgentPayload> {
    let (model_override, effort_override) = role_model_effort(agent_role);
    let resolved = image_resolver::resolve(cfg, agent_role);
    image_resolver::ensure_image(cfg, &resolved)?;
    let bundled_roles = cfg.home_root.join("roles");
    let role_prompt_text = match project_config::resolve_role_prompt(
        agent_role,
        clone_path,
        &bundled_roles,
    ) {
        Some(p) => std::fs::read_to_string(&p)
            .map_err(|e| anyhow::anyhow!("read role prompt {}: {}", p.display(), e))?,
        None => String::new(),
    };
    let meta_text = project_config::resolve_role_prompt("_meta", clone_path, &bundled_roles)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let role_prompt = if meta_text.is_empty() {
        role_prompt_text
    } else if role_prompt_text.is_empty() {
        meta_text
    } else {
        format!("{}\n\n---\n\n{}", meta_text, role_prompt_text)
    };

    Ok(StartAgentPayload {
        name: agent_name.to_string(),
        project_path: clone_path.to_string_lossy().to_string(),
        agent_dir: agent_dir.to_string_lossy().to_string(),
        role_memory_dir: role_memory_dir.to_string_lossy().to_string(),
        role_prompt,
        seed_credentials: cfg
            .seed_dir
            .join(".credentials.json")
            .to_string_lossy()
            .to_string(),
        role: agent_role.to_string(),
        mode: "long-running".to_string(),
        prompt: initial_prompt,
        orchestrator_port: cfg.orchestrator_port,
        mcp_port: cfg.mcp_port,
        dolt_port,
        image_name: resolved.image_name,
        network_name: cfg.network_name.clone(),
        extra_mounts: vec![],
        model: model_override,
        effort: effort_override,
        resume_session,
    })
}

/// The repo's integration branch to fork work from: `origin/HEAD` if a remote
/// default is configured, otherwise whichever of `main`/`master` exists locally.
/// Never the current branch — a team always branches off main/master unless the
/// caller passes an explicit `--base`.
fn default_base_branch(root: &Path) -> Option<String> {
    let remote_head = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().strip_prefix("origin/").map(str::to_string));
    if remote_head.is_some() {
        return remote_head;
    }
    ["main", "master"].into_iter().find(|b| local_branch_exists(root, b)).map(str::to_string)
}

fn local_branch_exists(root: &Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn cmd_spawn(
    cfg: &Config,
    ticket_id: &str,
    base: Option<&str>,
    maintenance: bool,
) -> Result<()> {
    // Project must be bd-enabled (same check as run-agent).
    let bd_marker = cfg.project_root.join(".beads").join("config.yaml");
    if !bd_marker.is_file() {
        bail!(
            "Project '{}' is not bd-enabled. Run `bd init` from the project root, then retry.",
            cfg.project_root.display()
        );
    }

    let base_branch = match base {
        Some(b) => b.to_string(),
        None => default_base_branch(&cfg.project_root).ok_or_else(|| {
            anyhow::anyhow!(
                "Could not detect a default branch (main/master) in '{}'; pass --base explicitly.",
                cfg.project_root.display()
            )
        })?,
    };
    println!("==> Base branch: {}", base_branch);

    // Try to refresh seed credentials from the macOS keychain so we don't
    // ship stale OAuth tokens into the team. Failures here are non-fatal —
    // ensure_credentials below will surface a real problem if the seed file
    // is missing entirely.
    match crate::auth::refresh_credentials_from_keychain(&cfg.seed_dir) {
        Ok(true) => println!("==> Refreshed credentials from keychain"),
        Ok(false) => {} // not macOS, or keychain inaccessible — fall through
        Err(e) => eprintln!("==> Warning: keychain refresh failed: {}", e),
    }

    let pcfg = cfg.to_project_config(None);
    project_config::ensure_credentials(&pcfg)?;

    // Per-agent images are built lazily inside build_payload_for_team_agent.
    // We still ensure the bundled base exists upfront so the first agent's
    // FROM clause resolves locally without surprising the user with a slow
    // first build right after the team manifest is written.
    image_resolver::ensure_base_image(cfg)?;

    container::ensure_network(&cfg.network_name)?;
    services::ensure_orchestrator(cfg)?;
    let dolt_port = services::ensure_dolt(&cfg.project_root)?;

    let mut mgr = open_manager(cfg)?;
    let roles = mvt_roles(maintenance);

    println!("==> Provisioning team for ticket '{}'", ticket_id);
    let team = mgr
        .create_team(SpawnSpec {
            ticket_id: ticket_id.to_string(),
            base_branch,
            roles,
        })
        .map_err(|e| anyhow::anyhow!("create team: {}", e))?
        .clone();

    println!("==> Team {}", team.id);
    println!("    clone:     {}", team.clone_path.display());
    println!("    branch:    {} (from {})", team.work_branch, team.base_branch);

    // Per-agent state under .teams/<id>/<role>/.claude/. Seed each fresh
    // dir with the same .claude-container/ contents that single-agent runs
    // get, so theme picker and trust-prompt are pre-accepted and the agent
    // doesn't hang on first-run setup.
    for agent in &team.agents {
        let agent_dir = mgr
            .agent_state_dir(&team.id, &agent.role)
            .join(".claude");
        if !agent_dir.exists() {
            project_config::seed_agent_state_dir(&cfg.seed_dir, &agent_dir)
                .with_context(|| format!("seed agent state dir {}", agent_dir.display()))?;
        } else {
            // Resume path: state already populated. Refresh credentials only.
            let creds_dest = agent_dir.join(".credentials.json");
            let _ = std::fs::remove_file(&creds_dest);
            let _ = std::fs::copy(cfg.seed_dir.join(".credentials.json"), &creds_dest);
        }

        let role_memory_dir = project_config::setup_role_memory_dir(&pcfg, &agent.role)?;
        let initial_prompt = build_initial_prompt(&team.id, &team.ticket_id, &agent.role);

        let clone_path = mgr
            .clone_path(&team.id)
            .ok_or_else(|| anyhow::anyhow!("no clone for team {}", team.id))?
            .to_path_buf();

        let payload = build_payload_for_team_agent(
            cfg,
            &clone_path,
            &agent.role,
            &agent.name,
            agent_dir,
            role_memory_dir,
            dolt_port,
            initial_prompt,
            false,
        )?;

        println!("==> Launching {} ({})", agent.name, agent.role);
        container::launch_long_running(&payload)?;
    }

    mgr.set_state(&team.id, TeamState::Active, None)
        .map_err(|e| anyhow::anyhow!("set state: {}", e))?;

    println!();
    println!("==> Team {} active.", team.id);
    println!("    Attach: tmux attach -t orchestrator");
    println!("    Suspend: agent team suspend {}", team.id);
    println!("    Status:  agent team status {}", team.id);

    Ok(())
}

pub fn cmd_suspend(cfg: &Config, team_id: &str, reason: Option<String>) -> Result<()> {
    let mut mgr = open_manager(cfg)?;
    let team = mgr
        .get(team_id)
        .ok_or_else(|| anyhow::anyhow!("team '{}' not found", team_id))?
        .clone();

    println!("==> Suspending team {}", team.id);
    mgr.set_state(&team.id, TeamState::Suspending, reason.clone())
        .map_err(|e| anyhow::anyhow!("set state: {}", e))?;

    // Kill the containers. State on disk (mounted from .teams/<id>/<role>/.claude)
    // is already persistent — no separate snapshot step needed.
    for agent in &team.agents {
        let _ = std::process::Command::new("podman")
            .args(["rm", "-f", &agent.name])
            .status();
    }

    // Close the team's tmux windows. Each agent's window is named after its
    // container; closing them keeps the orchestrator session tidy.
    for agent in &team.agents {
        let target = format!("orchestrator:{}", agent.name);
        let _ = std::process::Command::new("tmux")
            .args(["kill-window", "-t", &target])
            .status();
    }

    mgr.set_state(&team.id, TeamState::Suspended, reason)
        .map_err(|e| anyhow::anyhow!("set state: {}", e))?;

    println!("    State: suspended. Resume with: agent team resume {}", team.id);
    Ok(())
}

pub fn cmd_resume(cfg: &Config, team_id: &str, only_role: Option<String>) -> Result<()> {
    let _ = crate::auth::refresh_credentials_from_keychain(&cfg.seed_dir);
    let pcfg = cfg.to_project_config(None);
    project_config::ensure_credentials(&pcfg)?;
    container::ensure_network(&cfg.network_name)?;
    services::ensure_orchestrator(cfg)?;
    let dolt_port = services::ensure_dolt(&cfg.project_root)?;

    let mut mgr = open_manager(cfg)?;
    let team = mgr
        .get(team_id)
        .ok_or_else(|| anyhow::anyhow!("team '{}' not found", team_id))?
        .clone();

    if team.state != TeamState::Suspended && team.state != TeamState::Active {
        bail!(
            "team '{}' is in state {:?}; cannot resume",
            team_id,
            team.state
        );
    }

    println!("==> Resuming team {}", team.id);

    let to_resume: Vec<_> = team
        .agents
        .iter()
        .filter(|a| only_role.as_deref().map_or(true, |r| r == a.role))
        .collect();

    if to_resume.is_empty() {
        bail!("no matching role to resume");
    }

    for agent in to_resume {
        let agent_dir = mgr
            .agent_state_dir(&team.id, &agent.role)
            .join(".claude");
        // Refresh credentials in case they were rotated since suspend.
        let creds_dest = agent_dir.join(".credentials.json");
        let _ = std::fs::remove_file(&creds_dest);
        let _ = std::fs::copy(cfg.seed_dir.join(".credentials.json"), &creds_dest);

        let role_memory_dir = project_config::setup_role_memory_dir(&pcfg, &agent.role)?;

        let primer = build_resume_prompt(&team.id, &team.ticket_id, &agent.role);
        let resume_session = compute_resume_session(&agent.role, true);

        let clone_path = mgr
            .clone_path(&team.id)
            .ok_or_else(|| anyhow::anyhow!("no clone for team {}", team.id))?
            .to_path_buf();

        let payload = build_payload_for_team_agent(
            cfg,
            &clone_path,
            &agent.role,
            &agent.name,
            agent_dir,
            role_memory_dir,
            dolt_port,
            primer,
            resume_session,
        )?;

        println!("==> Resuming {} ({})", agent.name, agent.role);
        container::launch_long_running(&payload)?;
    }

    mgr.set_state(&team.id, TeamState::Active, None)
        .map_err(|e| anyhow::anyhow!("set state: {}", e))?;

    println!("    State: active.");
    Ok(())
}

/// `agent team integrate <id> [--merge]`. Host-mediated, PR-free integration:
/// show the work branch's diff vs base for review (default), or merge it into
/// base with `--merge`. The merge runs in the canonical repo (project root),
/// the only place with write access — agents never push here.
///
/// Before diff/merge, fetches the producer role's branch from its clone into
/// the canonical repo so the canonical repo has the latest commits.
pub fn cmd_integrate(cfg: &Config, team_id: &str, merge: bool) -> Result<()> {
    let mgr = open_manager(cfg)?;
    let team = mgr
        .get(team_id)
        .ok_or_else(|| anyhow::anyhow!("team '{}' not found", team_id))?;

    // Fetch the team's work branch from the shared clone into the canonical repo.
    mgr.fetch_team_branch(team_id)
        .map_err(|e| anyhow::anyhow!("fetch team branch: {}", e))?;

    let spec = IntegrateSpec {
        team_id: team.id.clone(),
        ticket_id: team.ticket_id.clone(),
        base_branch: team.base_branch.clone(),
        work_branch: team.work_branch.clone(),
    };
    let mode = if merge {
        IntegrateMode::Merge
    } else {
        IntegrateMode::Check
    };

    let report = integration::integrate(&RealMergeOps, &cfg.project_root, &spec, mode)
        .map_err(|e| anyhow::anyhow!("integrate: {}", e))?;

    println!("==> {} ({}): {} → {}", team.id, team.ticket_id, report.branch, report.base);
    println!("--- diffstat ---");
    println!("{}", report.diff_stat.trim_end());
    if let Some(diff) = &report.diff {
        println!("--- diff ---");
        println!("{}", diff);
        println!();
        println!("Review the diff above. To integrate: agent team integrate {} --merge", team.id);
    }
    if report.merged {
        println!("==> Merged {} into {}.", report.branch, report.base);
    }
    Ok(())
}

pub fn cmd_kill(cfg: &Config, team_id: &str, archive: bool) -> Result<()> {
    let mut mgr = open_manager(cfg)?;
    let team = mgr
        .get(team_id)
        .ok_or_else(|| anyhow::anyhow!("team '{}' not found", team_id))?
        .clone();

    println!("==> Killing team {} (archive={})", team.id, archive);
    for agent in &team.agents {
        let _ = std::process::Command::new("podman")
            .args(["rm", "-f", &agent.name])
            .status();
        let target = format!("orchestrator:{}", agent.name);
        let _ = std::process::Command::new("tmux")
            .args(["kill-window", "-t", &target])
            .status();
    }
    mgr.teardown(&team.id, archive)
        .map_err(|e| anyhow::anyhow!("teardown: {}", e))?;
    println!("    Team gone.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_prompt_producer_gets_prior_context_message() {
        for role in &["feature-producer", "maintenance-producer"] {
            let prompt = build_resume_prompt("t-team", "bd-42", role);
            assert!(
                prompt.contains("prior context intact"),
                "{role} resume prompt must mention 'prior context intact'; got: {prompt}"
            );
            assert!(
                !prompt.contains("not retained"),
                "{role} resume prompt must not say 'not retained'; got: {prompt}"
            );
        }
    }

    #[test]
    fn resume_prompt_non_producer_gets_fresh_context_message() {
        for role in &["review-agent", "planner"] {
            let prompt = build_resume_prompt("t-team", "bd-42", role);
            assert!(
                prompt.contains("not retained"),
                "{role} resume prompt must say 'not retained'; got: {prompt}"
            );
            assert!(
                !prompt.contains("prior context intact"),
                "{role} resume prompt must not say 'prior context intact'; got: {prompt}"
            );
        }
    }

    #[test]
    fn compute_resume_session_spawn_always_false() {
        for role in &["planner", "feature-producer", "maintenance-producer", "review-agent", "unknown"] {
            assert!(
                !compute_resume_session(role, false),
                "spawn (is_resume=false) must yield false for {role}"
            );
        }
    }

    #[test]
    fn compute_resume_session_resume_per_role_policy() {
        assert!(compute_resume_session("feature-producer", true), "feature-producer must get resume_session=true on resume");
        assert!(compute_resume_session("maintenance-producer", true), "maintenance-producer must get resume_session=true on resume");
        assert!(!compute_resume_session("review-agent", true), "review-agent must get resume_session=false on resume");
        assert!(!compute_resume_session("planner", true), "planner must get resume_session=false on resume");
        assert!(!compute_resume_session("unknown-role", true), "unknown role must get resume_session=false on resume");
    }

    #[test]
    fn producer_primer_has_no_git_push_or_gh_pr_create() {
        for role in &["feature-producer", "maintenance-producer"] {
            let prompt = build_initial_prompt("t-team", "bd-42", role);
            assert!(
                !prompt.contains("git_push"),
                "{role} primer must not mention git_push"
            );
            assert!(
                !prompt.contains("gh_pr_create"),
                "{role} primer must not mention gh_pr_create"
            );
            assert!(
                prompt.contains("message_agent"),
                "{role} primer must mention message_agent"
            );
            assert!(
                prompt.contains("ready for review"),
                "{role} primer must contain 'ready for review'"
            );
        }
    }

    #[test]
    fn reviewer_primer_has_no_gh_pr_create_and_has_approved() {
        let prompt = build_initial_prompt("t-team", "bd-42", "review-agent");
        assert!(
            !prompt.contains("gh_pr_create"),
            "reviewer primer must not mention gh_pr_create"
        );
        assert!(
            prompt.contains("approved"),
            "reviewer primer must contain 'approved'"
        );
        assert!(
            prompt.contains("findings as beads"),
            "reviewer primer must mention 'findings as beads'"
        );
    }

    #[test]
    fn planner_primer_still_works() {
        let prompt = build_initial_prompt("t-team", "bd-42", "planner");
        assert!(prompt.contains("PLANNER"));
        assert!(prompt.contains("bd-42"));
    }
}
