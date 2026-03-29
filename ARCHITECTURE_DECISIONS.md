# Architecture Decisions

## Single Rust Binary

The orchestrator is a single Rust binary that handles all host-side responsibilities:
- **WebSocket server** (port 9800) for agent registration and event routing
- **HTTP MCP server** (port 9801) for Claude Code tool calls from containers
- **TUI dashboard** (ratatui) for human interaction
- **Agent registry** for discovery and inter-agent messaging
- **Permission checker** for role-based access control

This replaced an earlier architecture with a separate Node.js bridge process. The merge eliminates stale-process bugs and the Node.js host dependency.

## Container as Security Boundary

Agents run with `--dangerously-skip-permissions` inside containers. The container IS the security boundary. This means:
- Full freedom inside (file access, code execution, package installs)
- No host access except through the orchestrator's permission-checked MCP tools
- Podman runs rootless with `--cap-drop=ALL` plus selective capabilities

## MCP HTTP Transport

Claude Code in containers connects to the orchestrator's MCP server via HTTP (`http://host.containers.internal:9801/mcp`). This is the MCP Streamable HTTP protocol: JSON-RPC 2.0 requests, SSE responses.

**Known limitation**: Claude Code has a ~60s timeout on MCP tool calls. Human-in-the-loop tools like `ask_user` can exceed this if the user takes too long to respond. A fix is tracked in beads (`agent-in-docker-1gd`).

## `--dangerously-skip-permissions`

Claude Code runs with `--dangerously-skip-permissions` inside containers. This is the core design -- the container IS the security boundary, not Claude Code's permission system. The whole point is to give the agent full freedom inside the sandbox.

Do not replace this with `--permission-mode dontAsk` or other alternatives.

## Privilege Dropping and UID Matching

`--dangerously-skip-permissions` refuses to run as root. The entrypoint runs as root for setup, then drops to a non-root user before starting Claude Code.

The non-root user must have the same uid as the host user (501 on macOS) so that bind-mounted files are readable/writable without permission issues. The entrypoint creates a user with the matching uid rather than using the container's default `node` user (uid 1000).

## Mount Permissions

- `/workspace` -- read-write, contains the project the agent works on
- `/root/.claude` -- read-write, agent's Claude Code config and credentials
- `.beads/` directory inside workspace -- read-write, beads auto-starts dolt locally per command

## Agent Config Directories

```
.claude-container/          # Seed directory (shared OAuth credentials)
  .credentials.json         # From 'run-agent.sh login'
  .claude.json              # Claude Code config
  backups/                  # Config backups

.claude-agents/             # Per-agent directories
  Alice/                    # Named agent (persistent across runs)
    .credentials.json       # Copied from seed on each launch
    .claude.json            # Agent's own config (evolves over time)
  ephemeral-agent-123/      # Ephemeral (deleted on exit)
```

Named agents get persistent directories that survive across container restarts. Credentials are always copied fresh from the seed directory on launch (not symlinked, because host paths don't exist inside the container).

## Beads and Dolt

Beads (`bd`) is the issue tracker. It requires a Dolt database server.

**Host side**: Dolt runs on the host machine (auto-started by `bd` on a dynamic port). Beads connects to it directly.

**Container side**: Agents that need beads access must connect to the host's Dolt server over the network. The configuration requires:
- `--server-host=host.containers.internal` to reach the host from inside the container
- `--server-port=<port>` matching the host's dolt server port

The entrypoint passes `DOLT_HOST` and `DOLT_PORT` environment variables to containers. Agents use `bd --server-host=$DOLT_HOST --server-port=$DOLT_PORT` or configure these in `.beads/config.yaml`.

**Important**: Beads should NOT try to auto-start its own dolt server inside the container. It should only connect to the host's existing server. The `.beads/` directory in the workspace is bind-mounted and may contain files owned by the host uid.

## Long-Running Agents via Host tmux

Named agents run Claude Code interactively inside host-side tmux windows (session `agents`, one window per agent). This means:
- Users can attach/detach from any agent: `tmux attach -t agents`
- The container runs with `-it` (interactive TTY) inside the tmux pane
- The CLI auto-accepts Claude Code's bypass permissions dialog via `tmux send-keys`
- No tmux inside the container -- the container is just `podman run -it`

## Role-Based Permissions

Roles are defined in `roles/*.yml`. Each role specifies:
- **Capabilities**: which tool categories are enabled (file_read, git_push, etc.)
- **Path patterns**: which host paths can be read (with glob matching)
- **Deny patterns**: always-blocked paths (checked before allow patterns)
- **Remote restrictions**: which git remotes can be pushed to

Hardcoded security denials (cannot be overridden by role config):
- SSH private keys (`~/.ssh/id_*`)
- AWS credentials (`~/.aws/credentials`)
- GCloud credentials (`~/.config/gcloud/application_default_credentials.json`)
- Claude credentials (`~/.claude/.credentials.json`)
