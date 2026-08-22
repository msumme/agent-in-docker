# agent-in-docker

Run LLM code agents inside Podman containers with full internal freedom but
restricted host access. The container boundary is the security model -- agents
run with `--dangerously-skip-permissions` inside, while a single Rust
orchestrator on the host mediates every external action (host file reads, git
pushes, PR creation) through a human-approved TUI dashboard.

Beyond single agents, the orchestrator runs **teams**: a
planner → producer → reviewer → human-merge pipeline scoped to one
[beads](https://github.com/steveyegge/beads) (`bd`) ticket, each role in its
own fresh container and git worktree.

## How it works

```
Host (Rust orchestrator + agent CLI)        Podman containers
┌────────────────────────────────┐          ┌─────────────────────┐
│  Orchestrator                  │          │  Claude Code         │
│  ├─ WebSocket server  :9800    │◄─── ws ──│  (per-role agent,    │
│  ├─ MCP HTTP server   :9801    │◄── http ─│   --skip-permissions)│
│  ├─ TUI dashboard              │          │                      │
│  ├─ Permission checker (roles) │          │  Tools: python3, git,│
│  ├─ Agent + Team managers      │          │  chromium, bd, gh    │
│  └─ PR watcher (gh polling)    │          │                      │
│                                │          │  /workspace (mount)  │
│  agent CLI: run / login / team │          └─────────────────────┘
│  Agents run in host tmux       │
└────────────────────────────────┘
```

1. The **orchestrator** runs on your host: a WebSocket server (agent registry +
   message routing, :9800), an MCP HTTP server (host-mediated tools, :9801), a
   ratatui TUI dashboard, a role-based permission checker, agent/team lifecycle
   managers, and a GitHub PR watcher.
2. The **`agent` CLI** launches containers, manages tmux sessions and git
   worktrees, copies credentials, and spawns/suspends/resumes teams.
3. Your project is bind-mounted at `/workspace` -- the only writable mount.
4. Containerized Claude Code connects to the MCP server for host-mediated tools
   and (for named/team agents) registers over WebSocket for discovery and
   inter-agent messaging.
5. Agents run in host-side tmux windows -- attach, interact, detach freely.

## Prerequisites

- [Podman](https://podman.io/) (rootless)
- [Rust](https://rustup.rs/) (to build the orchestrator + CLI)
- [tmux](https://github.com/tmux/tmux) (long-running agents and teams)
- [`gh`](https://cli.github.com/) on the host (PR creation + the PR watcher)

## Quick start

```bash
git clone https://github.com/msumme/agent-in-docker.git
cd agent-in-docker

# Build the orchestrator + CLI (workspace of four crates)
cd orchestrator && cargo build && cd ..

# Build the base container image
podman build -f Containerfile.base -t agent-in-docker .

# Authenticate Claude Code once (interactive /login + OAuth in browser)
./run-agent.sh login

# Run a single agent
./run-agent.sh . "Fix the failing tests"
```

`run-agent.sh` is a thin wrapper over the `agent` binary
(`orchestrator/target/debug/agent`). For team commands, call the binary directly.

## Single agents

The CLI is `agent run <project-path> "<prompt>" [options]`:

| Option | Meaning |
|--------|---------|
| `--role <role>` | Permissions, memory bucket, and default role-prompt. Defaults to `maintenance-producer`. |
| `--role-prompt <name\|path>` | Override the role-prompt file (bare name resolved against project/user/bundled role dirs, or a path). |
| `--name <name>` | Named, persistent, long-running agent in its own tmux window. |
| `--oneshot` | Run once and exit even if named. |
| `--build` | Force rebuild of the container image. |

**Ephemeral (default)** -- runs the prompt, prints the response, exits:
```bash
./run-agent.sh ./my-app "Add input validation to the signup form"
```

**Named long-running** -- stays alive for interactive use in tmux:
```bash
./run-agent.sh ./my-app "You are a code agent" --name coder --role feature-producer
# Then: tmux attach -t agents   (Ctrl-b n / Ctrl-b p to switch windows)
```

## Teams

A team is a PR-scoped group of agents working one `bd` ticket through a fixed
lifecycle: **planner → producer → reviewer → human merge**. Each role runs in a
fresh container against a dedicated git worktree (`<name>/bd-<id>` branch), so a
rejected change re-spawns the producer with clean context -- no
"producer-prejudice" on review. Team state lives host-side under `.teams/<id>/`
(manifest) and `.teams-worktrees/`.

```bash
AGENT=orchestrator/target/debug/agent

# Spawn a team for a ticket (provisions worktree + planner/producer/reviewer)
$AGENT team spawn agent-in-docker-0fw.2 --base main
$AGENT team spawn <ticket> --maintenance      # use maintenance-producer

$AGENT team list                     # all known teams (active + suspended)
$AGENT team status <team-id>         # one team's manifest
$AGENT team suspend <team-id> --reason "waiting on review"   # kill containers, keep state
$AGENT team resume  <team-id> [--role <role>]                # restart containers
$AGENT team kill    <team-id> [--no-archive]                 # teardown + remove worktree
```

The **PR watcher** polls the team's GitHub PR and tears the team down on merge,
flagging PRs that close without merging. Context discipline is built in: the
orchestrator dispatches `/compact` at ~60% context or at natural breaks (PR
opened, ticket closed, suspend).

## TUI dashboard

Shows connected agents, pending host-action requests, teams, and an activity
log. Every gated host action (file read, git push, PR create) surfaces here for
human approval before it executes.

```
┌─ Agents ──────────┬─ Activity Log ────────────────────┐
│ ● coder           │ + coder (feature-producer) joined  │
│   role: producer  │ [coder] git_push origin -> approved│
├─ Pending Requests ┴────────────────────────────────────┤
│ > [coder] git_push: origin  feat/bd-42                  │
│   y: approve   n: deny                                  │
└────────────────────────────────────────────────────────┘
 Tab: switch | Up/Down: navigate | Enter/y/n: act | a: attach | q: quit
```

| Key | Action |
|-----|--------|
| Tab | Switch focus between panels |
| Up/Down | Navigate |
| Enter | Submit answer / approve request |
| y/n | Approve / deny a gated request |
| a | Attach to the selected agent's tmux session |
| q | Quit (blocked while requests are pending) |

> Note: git pushes from a **team** agent to its own work branch are
> auto-approved (no TUI prompt) -- the work-branch match is read from the
> host-side team manifest.

## Architecture

### Rust workspace (`orchestrator/`)

Four crates:

- **`core`** -- WebSocket server, MCP HTTP server (axum), agent registry,
  permission checker, team + agent lifecycle managers, PR watcher. All business
  logic, dependency-injected for testing.
- **`tui`** -- ratatui dashboard. Consumes `OrchestratorEvent`s, sends
  `TuiCommand`s back.
- **`cli`** -- the `agent` binary (`run`, `login`, `team …`). Manages container
  lifecycle, tmux sessions, worktrees, credential copying, dialog auto-accept.
- **`entrypoint`** -- the in-container init binary (Rust, replacing the old bash
  entrypoint): restores Claude config, wires MCP, registers over WebSocket,
  launches Claude Code.

### MCP tools

Exposed to agents over HTTP MCP (`/mcp`). Host-gated tools return
`NeedsApproval` and block on TUI approval; the rest resolve immediately.

| Tool | Description |
|------|-------------|
| `read_host_file` | Read a host file (permission-checked, TUI-approved) |
| `git_push` | Push using host git/SSH credentials (TUI-approved; auto-approved for team work branches) |
| `gh_pr_create` / `gh_pr_view` | Create / view a GitHub PR via host `gh` |
| `list_agents` / `message_agent` | Discover and message other connected agents |
| `team_spawn` / `team_suspend` / `team_resume` / `team_complete` / `team_kill` | Team lifecycle (privileged) |

### Roles

Defined in `roles/*.yml` (capabilities + path/remote allowlists) paired with
`roles/*.md` (the role prompt). Shipped roles: `planner`, `feature-producer`,
`maintenance-producer`, `review-agent`. Shared coding/coordination standards
live in `roles/_meta.md`.

```yaml
name: feature-producer
capabilities:
  file_read: true
  git_push: true
  gh_pr_create: true
file_read_paths:
  - "${HOME}/.gitconfig"
file_read_deny_paths:
  - "**/*.pem"
  - "**/*.key"
git_push_remotes:
  - "origin"
```

Hardcoded denials (never overridable by any role): SSH private keys
(`**/.ssh/id_*`), AWS/GCloud credentials, and Claude credentials.

### Container

`Containerfile.base` (Alpine) ships Claude Code, Python 3, Git, `gh`, Chromium
(Playwright), beads (`bd`) + dolt, bash, curl. Per-stack variants live in
`variants/` (`minimal`, `python-data`, `rust-dev`).

### Coordination (beads)

Teams coordinate through a shared `bd` database, not chat: branch-per-ticket,
`bd merge-slot` to serialize integration, findings filed as issues. Each project
runs its own dolt server on a fixed port (`.beads/dolt-server.port`); containers
connect to host dolt over the network.

## Security model

- `--dangerously-skip-permissions` **inside** the container, always -- the
  container, not Claude Code's permission prompt, is the boundary. Claude Code
  runs as root with `IS_SANDBOX=1`.
- `--cap-drop=ALL` plus a minimal cap set; no `--privileged`, no host
  PID/network namespace; rootless Podman.
- Workspace is the only writable bind mount.
- Host file reads, git pushes, and PR creation are permission-checked against
  the role and require human approval in the TUI (team work-branch pushes
  excepted). The checker is a first-line filter; the human approval gate and the
  container boundary are the real protections.

## Credentials

Agents use your Claude Max/Pro subscription (not API keys). Log in once:

```bash
./run-agent.sh login   # /login inside Claude Code, complete OAuth in browser
```

Credentials are stored in `.claude-container/` and copied into each agent's
config dir. Named agents get persistent dirs under `.agents/<name>/`, which
also hold that agent's own Claude session state (`projects/`) — scoped to
that agent instance, not shared across roles. Durable learnings are proposed
via `.agents/lessons/proposed/` (see `_meta.md`'s "Lessons" section) and
folded into `roles/` by a human at review, not accumulated in agent memory.

## Development

```bash
cd orchestrator && cargo test     # core + integration tests
cd orchestrator && cargo build    # builds all four crates
```

### DI patterns

- `EnvResolver`, `ShellOps`, `IdGenerator`, `TeamLookup`/`TeamOps`,
  `RequestExecutor` traits -- real implementations in production, fakes in tests.
- `McpState` keeps a `std::sync::Mutex` pending map resolvable from any thread.

## Known gaps

- **ask_user / 60s MCP timeout** -- Claude Code times out MCP tool calls at
  ~60s; a host action that waits longer than that for human approval can be lost
  (tracked in `agent-in-docker-1gd`).
- **Token expiry** -- OAuth tokens expire with no auto-refresh; re-run
  `./run-agent.sh login`.
- **No live output streaming** -- the TUI activity log is event-based; to watch
  an agent think, attach to its tmux window.
- **Single-host only** -- team manifests are host-local JSON; the design assumes
  one orchestrator process per project, not a distributed/replicated deployment.
</content>
