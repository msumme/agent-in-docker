//! `agent team {spawn,suspend,resume,list,status,kill}` subcommand.
//!
//! Teams are provisioned via TeamManager (worktree, manifest, state dirs)
//! and then 3 containers are launched against the worktree with team-scoped
//! state directories. Suspend and resume are container lifecycle operations
//! over the same mounts — the per-agent state is already persisted on disk
//! by virtue of being mounted from the host, so suspend = `podman rm -f` +
//! manifest state update, and resume = relaunch with the same mounts.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;

use orchestrator_core::integration::{self, IntegrateMode, IntegrateSpec, RealMergeOps};
use orchestrator_core::project_config;
use orchestrator_core::team_manager::{
    RealGitOps, SpawnSpec, Team, TeamManager, TeamState,
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
             Available host-bridge MCP tools (call them by name): \
             `git_push`, `gh_pr_create` (open a PR — required when spec is \
             complete and reviewer approves), `gh_pr_view`, `read_host_file`, \
             `list_agents`, `message_agent`. If a tool errors, report the \
             verbatim error — do NOT conclude the tool is unavailable.",
            team = team_id,
            ticket = ticket_id,
        ),
        "review-agent" => format!(
            "You are the REVIEWER on team {team} for bd ticket {ticket}. The team \
             has just spawned. The pipeline is planner → producer → you. There is \
             no PR yet.\n\n\
             Do NOT start reviewing yet. Acknowledge with one short line (e.g. \
             \"reviewer ready, waiting on PR\") and stop. You will be pinged via \
             message_agent when the producer opens a PR.",
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
        "TEAM", "STATE", "TICKET", "WORKTREE"
    );
    for t in teams {
        println!(
            "{:<32} {:<10} {:<28} {}",
            t.id,
            format!("{:?}", t.state).to_lowercase(),
            t.ticket_id,
            t.worktree_path.display()
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
    println!("worktree:     {}", team.worktree_path.display());
    println!("created:      {}", team.created_at);
    println!("last active:  {}", team.last_active);
    if let Some(reason) = &team.suspend_reason {
        println!("suspend why:  {}", reason);
    }
    if let Some(url) = &team.pr_url {
        println!("pr:           {}", url);
    }
    println!("agents:");
    for a in &team.agents {
        println!("  {:<6} {}", a.role, a.name);
    }
    Ok(())
}

/// Build the StartAgentPayload for one team agent — pointed at the team's
/// worktree, with a team-scoped agent_dir so each role's state stays under
/// `.teams/<team-id>/<role>/.claude/`. Resolves the role prompt with the
/// meta-prompt prepended, just like the regular Run flow.
///
/// Also mounts the project root at its host path so git worktree pointers
/// resolve inside the container (the worktree's `.git` file holds an
/// absolute host path; without this mirror mount, `git status` and friends
/// fail with "not a git repository"). This is the option-2 worktree fix.
fn build_payload_for_team_agent(
    cfg: &Config,
    team: &Team,
    agent_role: &str,
    agent_name: &str,
    agent_dir: PathBuf,
    role_memory_dir: PathBuf,
    dolt_port: Option<u16>,
    initial_prompt: String,
) -> Result<StartAgentPayload> {
    let (model_override, effort_override) = role_model_effort(agent_role);
    let resolved = image_resolver::resolve(cfg, agent_role);
    image_resolver::ensure_image(cfg, &resolved)?;
    let bundled_roles = cfg.project_root.join("roles");
    // Resolve role prompt by name → bundled .md.
    let role_prompt_text = match project_config::resolve_role_prompt(
        agent_role,
        &team.worktree_path,
        &bundled_roles,
    ) {
        Some(p) => std::fs::read_to_string(&p)
            .map_err(|e| anyhow::anyhow!("read role prompt {}: {}", p.display(), e))?,
        None => String::new(),
    };
    // Prepend meta.
    let meta_text = project_config::resolve_role_prompt("_meta", &team.worktree_path, &bundled_roles)
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
        project_path: team.worktree_path.to_string_lossy().to_string(),
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
        // Mirror the project root at its host path. The worktree's `.git`
        // file points to `<host-project>/.git/worktrees/<id>` — that path
        // must exist inside the container for `git status`/`commit`/`push`
        // to resolve the worktree pointer.
        extra_mounts: vec![(
            cfg.project_root.to_string_lossy().to_string(),
            cfg.project_root.to_string_lossy().to_string(),
        )],
        model: model_override,
        effort: effort_override,
    })
}

pub fn cmd_spawn(
    cfg: &Config,
    ticket_id: &str,
    base_branch: &str,
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
            base_branch: base_branch.to_string(),
            roles,
        })
        .map_err(|e| anyhow::anyhow!("create team: {}", e))?
        .clone();

    println!("==> Team {}", team.id);
    println!("    worktree:  {}", team.worktree_path.display());
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

        let payload = build_payload_for_team_agent(
            cfg,
            &team,
            &agent.role,
            &agent.name,
            agent_dir,
            role_memory_dir,
            dolt_port,
            initial_prompt,
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

        let primer = format!(
            "[resume primer] You are resuming team {team} on bd ticket {ticket} as the \
             {role}. Your prior conversation state is loaded. Read `bd show {ticket}` \
             for current status and continue.",
            team = team.id,
            ticket = team.ticket_id,
            role = agent.role,
        );

        let payload = build_payload_for_team_agent(
            cfg,
            &team,
            &agent.role,
            &agent.name,
            agent_dir,
            role_memory_dir,
            dolt_port,
            primer,
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
pub fn cmd_integrate(cfg: &Config, team_id: &str, merge: bool) -> Result<()> {
    let mgr = open_manager(cfg)?;
    let team = mgr
        .get(team_id)
        .ok_or_else(|| anyhow::anyhow!("team '{}' not found", team_id))?;

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
