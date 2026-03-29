# agent-in-docker

Run LLM code agents inside Podman containers with full internal freedom but restricted host access. The container boundary is the security model -- agents run with `--dangerously-skip-permissions` inside, while a single Rust orchestrator binary on the host mediates all external actions through a TUI dashboard.

## How it works

```
Host (single Rust binary)               Podman Container
┌──────────────────────────┐            ┌─────────────────────┐
│  Orchestrator            │            │  Claude Code         │
│  ├─ TUI Dashboard        │◄── http ──│  (interactive)       │
│  ├─ MCP HTTP Server      │            │                      │
│  ├─ WebSocket Server     │            │  Tools: python3, git,│
│  ├─ Permission Checker   │            │  chromium, beads,    │
│  └─ Agent Registry       │            │  dolt                │
│                          │            │                      │
│  Agents run in tmux      │            │  /workspace (mount)  │
│  on the host             │            └─────────────────────┘
└──────────────────────────┘
```

1. The **orchestrator** runs on your host -- a single Rust binary serving a TUI dashboard, an MCP HTTP server, and a WebSocket server
2. Your project directory is bind-mounted into the container at `/workspace`
3. Claude Code in the container connects to the orchestrator's MCP server for host-mediated tools (`ask_user`, `read_host_file`, `git_push`, `list_agents`, `message_agent`)
4. Named agents run in host-side tmux windows -- attach, interact, detach freely

## Prerequisites

- [Podman](https://podman.io/) (rootless)
- [Rust](https://rustup.rs/) (for building the orchestrator)
- [tmux](https://github.com/tmux/tmux) (for long-running agents)

## Quick start

```bash
git clone https://github.com/msumme/agent-in-docker.git
cd agent-in-docker

# Build the orchestrator
cd orchestrator && cargo build && cd ..

# Build the container image
podman build -f Containerfile -t agent-in-docker .

# Authenticate (first time only -- opens Claude Code for /login)
./run-agent.sh login

# Run an agent
./run-agent.sh . "Fix the failing tests"
```

## Usage

```
./run-agent.sh <project-path> "<prompt>" [options]
./run-agent.sh login

Commands:
  login               Authenticate Claude Code (interactive)

Options:
  --role <role>       Agent role (default: code-agent)
  --name <name>       Named agent (persistent, long-running)
  --oneshot           Run once even if named
  --build             Force rebuild container image
```

### Agent modes

**Ephemeral (default)** -- runs the prompt, prints the response, exits:
```bash
./run-agent.sh ./my-app "Add input validation to the signup form"
```

**Named long-running** -- launches in a tmux window, stays alive for interactive use:
```bash
./run-agent.sh ./my-app "You are a code agent" --name coder
# Then: tmux attach -t agents
```

**Multiple agents** -- each gets its own tmux window:
```bash
./run-agent.sh ./my-app "Write the feature" --name coder
./run-agent.sh ./my-app "Review the code" --name reviewer --role review-agent
# Switch between them: Ctrl-b n / Ctrl-b p
```

### TUI dashboard

The orchestrator TUI shows connected agents, pending requests, and activity:

```
┌─ Agents ──────────┬─ Activity Log ────────────────────┐
│ ● coder           │ + coder (code-agent) connected     │
│   role: code-agent│ [coder] Q: color? -> A: blue       │
├─ Pending Requests ┴────────────────────────────────────┤
│ > [coder] user_prompt: Should I refactor this?          │
│ ┌ Answer (Enter to submit) ──────────────────────────┐ │
│ │ yes, go ahead                                      │ │
│ └────────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────┘
 Agents: 1 | Pending: 1 | Tab: switch | a: attach | q: quit
```

| Key | Action |
|-----|--------|
| Tab | Switch focus between Agents and Requests |
| Up/Down | Navigate |
| Enter | Submit answer (user_prompt) or approve (file_read, git_push) |
| y/n | Approve/deny file_read and git_push requests |
| a | Attach to selected agent's tmux session |
| q | Quit (only when no pending requests) |

## Architecture

### Single Rust binary

Three crates in `orchestrator/`:

- **`orchestrator-core`** -- WebSocket server, HTTP MCP server (axum), agent registry, permission checker, message routing. All business logic with DI for testing.
- **`orchestrator-tui`** -- ratatui terminal dashboard. Consumes events from core, sends commands back.
- **`agent-cli`** -- CLI binary (`agent run`, `agent login`). Manages container lifecycle, tmux sessions, auto-accepts dialogs.

### MCP tools

The orchestrator exposes these tools to agents via HTTP MCP (`/mcp` endpoint):

| Tool | Description |
|------|-------------|
| `ask_user` | Ask the host user a question via the TUI |
| `read_host_file` | Read a host file (permission-checked, requires TUI approval) |
| `git_push` | Push using host git/SSH credentials (requires TUI approval) |
| `list_agents` | List all connected agents and their roles |
| `message_agent` | Send a message to another connected agent |

### Container contents

Alpine-based image with:
- Claude Code CLI
- Python 3, Git
- Chromium (for Playwright browser automation)
- beads (`bd`) + dolt (issue tracking)
- bash, curl

### Permission system

Roles defined in `roles/*.yml`:

```yaml
name: code-agent
capabilities:
  file_read: true
  git_push: true
  user_prompt: true
  discover_agents: true
  message_agents: true
file_read_paths:
  - "${HOME}/.gitconfig"
  - "${HOME}/.ssh/config"
file_read_deny_paths:
  - "**/*.pem"
  - "**/*.key"
git_push_remotes:
  - "origin"
```

Hardcoded denials (never overridable): SSH private keys, AWS/GCloud credentials, Claude credentials.

## Development

```bash
cd orchestrator && cargo test    # 59 tests
```

### DI patterns

- `IdGenerator` trait -- UUID in production, sequential in tests
- `AgentRegistry` trait -- real server state in production, `NoOpRegistry` in tests
- `McpState` with `std::sync::Mutex` pending map -- resolvable from any thread

## Security model

Containers run with:
- `--cap-drop=ALL` + selective caps (NET_RAW, CHOWN, SETUID, SETGID, DAC_OVERRIDE)
- No `--privileged`, no host PID/network namespace
- Workspace mount is the only writable bind mount
- All host actions (file read, git push) require human approval in the TUI

Agents cannot access host files outside the mounted workspace, use host credentials directly, or escalate privileges outside the container.

## Credentials

Agents use your Claude Max/Pro subscription (not API keys). Login once:

```bash
./run-agent.sh login
# Use /login inside Claude Code, complete OAuth in browser
```

Credentials are stored in `.claude-container/` and copied to each agent's config directory. Named agents get persistent dirs under `.claude-agents/<name>/`.
