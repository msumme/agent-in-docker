# Architecture Decisions

## Overview

agent-in-docker runs LLM code agents inside Podman containers. The container is the security boundary -- agents have full freedom inside but restricted host access.

## Components

### 1. Rust Orchestrator Binary (`orchestrator`)

Single binary with three subsystems:

**WebSocket Server (port 9800)**
- Agents register with `{ type: "register", payload: { name, role } }`
- Handles: `user_prompt`, `file_read`, `git_push`, `discover`, `agent_message`
- Broadcasts `peer_joined`/`peer_left` on connect/disconnect
- TUI communicates via channels (`OrchestratorEvent` and `TuiCommand`)

**MCP HTTP Server (port 9801)**
- Claude Code in containers connects here for tool calls
- Implements MCP Streamable HTTP protocol (JSON-RPC over HTTP, SSE responses)
- Tools: `ask_user`, `read_host_file`, `git_push`, `list_agents`, `message_agent`
- Each request creates a pending oneshot channel, resolved by TUI interaction

**TUI Dashboard (ratatui)**
- Shows: agent list, pending requests, activity log
- Handles: text answers for `ask_user`, y/n approval for `file_read`/`git_push`
- Keybindings: Tab (switch panels), Enter (submit), y/n (approve/deny), a (attach), q (quit)
- Sends typed input as `SendTask` to selected agent when no pending requests

### 2. CLI Binary (`agent`)

Host-side launcher. Subcommands:
- `agent run <path> <prompt>` -- launch an agent in a container
- `agent login` -- authenticate Claude Code

Responsibilities:
- Reads credentials from `.claude-container/`
- Copies credentials into per-agent config dirs (`.agents/<name>/`)
- Ensures orchestrator is running (starts in tmux if needed)
- Ensures project's dolt server is running (for beads)
- Launches containers via podman, manages tmux sessions for named agents
- Auto-accepts bypass permissions dialog via `tmux send-keys`

### 3. Container (`Containerfile`)

Alpine-based image containing:
- Claude Code CLI (Node.js)
- Python 3, Git, Chromium (Playwright), bash, curl
- beads (`bd`) + dolt (issue tracking)
- `entrypoint.sh` (setup + launch)

### 4. Entrypoint (`scripts/entrypoint.sh`)

Runs inside the container at startup:
1. Symlinks/restores `~/.claude.json` from mount
2. Pre-accepts workspace trust (modifies JSON via `node -e`)
3. Sets `BEADS_DOLT_SERVER_HOST/PORT` env vars for host dolt connection
4. Generates MCP config pointing to `host.containers.internal:9801/mcp`
5. Detects workspace owner uid, creates matching user
6. Drops to that user via `su`, runs Claude Code with `--dangerously-skip-permissions`

## Data Flow

### Oneshot Agent
```
CLI → podman run → entrypoint → claude -p "prompt" → MCP HTTP → orchestrator → TUI
                                                                                 ↓
CLI ← podman exit ← claude exits ← MCP response ← orchestrator ← TUI (user answers)
```

### Named Long-Running Agent
```
CLI → tmux window → podman run -it → entrypoint → claude (interactive)
                                                      ↕
                                                 MCP HTTP ↔ orchestrator ↔ TUI
```
User attaches to agent via `tmux attach -t agents`, or interacts via TUI.

## Credential Flow

```
./run-agent.sh login
  → podman run -it (interactive Claude Code)
  → user runs /login, completes OAuth in browser
  → credentials saved to .claude-container/.credentials.json

./run-agent.sh . "prompt" --name X
  → CLI copies .claude-container/ → .agents/X/
  → container mounts .agents/X/ at /root/.claude
  → entrypoint symlinks .claude.json, creates uid-matched user
  → Claude Code reads credentials from /home/agent/.claude/
```

## Beads/Dolt Integration

Each project has its own dolt server on a fixed port stored in `.beads/dolt-server.port`.

- **Host**: `bd` auto-starts dolt pointing at `.beads/dolt/` data directory
- **Container**: CLI reads the port, ensures dolt is running, passes `DOLT_HOST` + `DOLT_PORT` env vars. Entrypoint sets `BEADS_DOLT_SERVER_HOST/PORT` so `bd` connects to host dolt over the network.

## Permission Model

Roles defined in `roles/*.yml` with:
- Capability flags (`file_read`, `git_push`, `user_prompt`, etc.)
- Path allow/deny globs for file reads
- Allowed git remotes
- Hardcoded denials: SSH keys, cloud credentials, Claude credentials

All host actions require TUI approval (human-in-the-loop).

## Claude Code Runs as Root with IS_SANDBOX=1

Claude Code runs with `--dangerously-skip-permissions` inside containers. This is the core design -- the container IS the security boundary, not Claude Code's permission system. Do not replace this with `--permission-mode dontAsk` or other alternatives.

Claude Code runs as **root** inside the container with `IS_SANDBOX=1` environment variable. This bypasses Claude Code's refusal to run `--dangerously-skip-permissions` as root. Running as root eliminates all file permission issues:
- No uid mismatch between host and container
- No adduser/su/privilege dropping
- No SETUID/CHOWN/DAC_OVERRIDE capabilities needed
- All bind-mounted files (workspace, .beads, .claude) accessible without permission errors

Do not add user creation, uid detection, or privilege dropping code.

## Security

- `--dangerously-skip-permissions` with `IS_SANDBOX=1` -- always
- `--cap-drop=ALL --cap-add=NET_RAW` -- minimal capabilities
- Rootless Podman (no daemon, no root on host)
- Workspace is the only writable bind mount from the project

## Known Issues and Gaps

### MCP clients invisible to TUI
Containers connect via MCP HTTP (port 9801) but never register as WebSocket agents (port 9800). The TUI's "Agents" panel only shows WS-connected agents. MCP HTTP clients are invisible -- the TUI shows "Agents: 0" even when containers are running and making tool calls. The pending requests still appear but without agent identity.

### ask_user timeout
Claude Code has a ~60s timeout on MCP tool calls. If the user takes longer than 60s to answer `ask_user` in the TUI, the response is lost. The MCP HTTP request times out on Claude Code's side before the TUI resolves the oneshot channel. Tracked in `agent-in-docker-1gd`.

### Entrypoint fragility
The bash entrypoint handles: JSON editing via `node -e`, uid detection via `stat`, user creation via `adduser`, privilege dropping via `su`. Being replaced by a Rust entrypoint binary (agent-in-docker-lnq.3). Most of the complexity goes away once Claude Code runs as root with IS_SANDBOX=1 (agent-in-docker-clp).

### No agent output streaming
The TUI has an "Activity Log" panel but no live streaming of agent output. You can only see what agents are doing by attaching to their tmux window. The orchestrator has an `AgentOutput` event type but nothing populates it.

### Disconnected registries
The WebSocket agent registry and MCP HTTP pending requests are separate systems. `list_agents` and `message_agent` MCP tools query the WS registry, which MCP-only clients aren't part of. Multi-agent coordination only works between WS-connected agents.

### Token expiry
OAuth tokens expire and there's no automatic refresh. When tokens expire, agents fail with auth errors. The user must manually run `./run-agent.sh login` to refresh.
