# Agent-in-Docker (Podman)

## Context
Run LLM-based code agents inside Podman containers with full internal freedom but restricted host access. The container boundary is the security model -- agents run with `--dangerously-skip-permissions` inside but cannot escalate privileges outside. Communication back to the host is mediated by a Rust-based orchestrator with a TUI dashboard, role-based permission checks, and human-in-the-loop approval for host actions.

## Architecture

```
Host (Rust)                             Podman Containers
┌──────────────────────────┐           ┌─────────────────────┐
│  run-agent.sh            │           │  entrypoint.sh      │
│       │                  │           │    ├─ bridge (TS/MCP)│──ws──┐
│       ▼                  │           │    └─ claude/agent   │      │
│  orchestrator            │           └─────────────────────┘      │
│  ┌────────────────────┐  │           ┌─────────────────────┐      │
│  │ TUI (ratatui)      │  │           │  another agent      │──ws──┤
│  │  ├─ agent list      │  │           └─────────────────────┘      │
│  │  ├─ request log     │  │                                        │
│  │  ├─ approve/deny    │◄─┼────────────────────────────────────────┘
│  │  └─ agent output    │  │
│  └────────────────────┘  │
│  ┌────────────────────┐  │
│  │ Core (lib)         │  │
│  │  ├─ ws server      │  │
│  │  ├─ permissions     │  │
│  │  ├─ agent registry  │  │
│  │  └─ message router  │  │
│  └────────────────────┘  │
└──────────────────────────┘
```

Key design: the orchestrator separates **core logic** (library) from **frontend** (TUI). The core exposes a clean API so future frontends (web UI, CLI, etc.) can plug in without rewriting business logic.

## Tech Stack
- **Orchestrator**: Rust (`tokio`, `tokio-tungstenite`, `ratatui`, `serde`, `crossterm`)
- **Bridge** (in-container): TypeScript (`@modelcontextprotocol/sdk`, `ws`)
- **Container runtime**: Podman (rootless)
- **CLI entry point**: Shell script

## Directory Structure

```
agent-in-docker/
├── run-agent.sh                        # CLI entry point
├── Containerfile                       # OCI image definition
├── compose.yml                         # Podman compose for networking
├── roles/
│   ├── code-agent.yml
│   └── review-agent.yml
│
├── orchestrator/                       # Rust workspace
│   ├── Cargo.toml                      # Workspace root
│   ├── crates/
│   │   ├── core/                       # Library: WS server, permissions, routing
│   │   │   ├── Cargo.toml
│   │   │   └── src/
│   │   │       ├── lib.rs
│   │   │       ├── server.rs           # WebSocket server + connection mgmt
│   │   │       ├── permissions.rs      # Role loading + permission checks
│   │   │       ├── registry.rs         # Agent registry + discovery
│   │   │       ├── router.rs           # Message routing
│   │   │       ├── handlers/
│   │   │       │   ├── mod.rs
│   │   │       │   ├── file_read.rs
│   │   │       │   ├── git_push.rs
│   │   │       │   ├── user_prompt.rs
│   │   │       │   └── discovery.rs
│   │   │       └── types.rs            # Message types, role definitions
│   │   └── tui/                        # Binary: ratatui dashboard
│   │       ├── Cargo.toml
│   │       └── src/
│   │           ├── main.rs
│   │           ├── app.rs              # App state + event loop
│   │           ├── ui.rs               # Layout + rendering
│   │           └── widgets/
│   │               ├── mod.rs
│   │               ├── agent_list.rs   # Connected agents panel
│   │               ├── request_log.rs  # Host action requests + approve/deny
│   │               └── agent_output.rs # Selected agent's output stream
│
├── bridge/                             # TypeScript (in-container)
│   ├── package.json
│   ├── tsconfig.json
│   └── src/
│       ├── index.ts                    # Entry: connect WS + start MCP server
│       ├── ws-client.ts                # WebSocket client to orchestrator
│       ├── mcp-server.ts               # MCP stdio server for agent
│       └── tools/
│           ├── ask-user.ts
│           ├── read-host-file.ts
│           ├── git-push.ts
│           ├── list-agents.ts
│           └── message-agent.ts
│
└── scripts/
    ├── build.sh                        # Build orchestrator + bridge
    └── entrypoint.sh                   # Container entrypoint
```

## WebSocket Protocol

JSON messages: `{ id, type, from, to?, payload }`

| Type | Direction | Purpose |
|---|---|---|
| `register` | container->host | Agent announces itself (name, role) |
| `register_ack` | host->container | Confirms registration + peer list |
| `user_prompt` / `_response` | bidirectional | Forward question to TUI for user |
| `file_read` / `_response` | bidirectional | Read host file (permission-checked) |
| `git_push` / `_response` | bidirectional | Push using host credentials |
| `discover` / `_response` | bidirectional | List connected agents |
| `agent_message` / `_delivery` | routed | Inter-agent messaging |
| `peer_joined` / `peer_left` | broadcast | Agent connect/disconnect |
| `error` | host->container | Permission denied / errors |

## TUI Dashboard

```
┌─ Agents ──────────┬─ Agent Output ────────────────────┐
│ ● code-agent-1    │ [code-agent-1] Reading files...   │
│   role: code-agent│ [code-agent-1] Found 3 tests      │
│   status: working │ [code-agent-1] Running npm test    │
│                   │                                    │
│ ○ review-agent-1  │                                    │
│   role: review    │                                    │
│   status: idle    │                                    │
├─ Pending Requests ┴────────────────────────────────────┤
│ [code-agent-1] file_read: ~/.gitconfig    [Y] [N]     │
│ [code-agent-1] git_push: origin/main      [Y] [N]     │
│ [code-agent-1] ask_user: "Should I refactor this?"    │
│   > [type your answer here]                            │
└────────────────────────────────────────────────────────┘
```

Features:
- Live agent list with status indicators
- Stream of agent output (selected agent)
- Pending host-action requests with approve/deny (y/n keys)
- User prompt input inline
- Keyboard navigation between panels

Core emits events (`AgentConnected`, `RequestReceived`, `AgentOutput`) via `tokio::sync::mpsc` channels. TUI consumes them. Future web UI would consume the same stream.

## MCP Tools (bridge exposes to agent)

| Tool | Input | Description |
|---|---|---|
| `ask_user` | `{ question }` | Ask the host user via TUI |
| `read_host_file` | `{ path }` | Read allowed host file |
| `git_push` | `{ remote?, branch? }` | Push via host credentials |
| `list_agents` | `{}` | List connected agents |
| `message_agent` | `{ agentId, message }` | Message another agent |

## Permission / Role System

```yaml
# roles/code-agent.yml
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

file_read_deny_paths:     # Checked first, always wins
  - "**/*.pem"
  - "**/*_rsa"
  - "**/*.key"
  - "**/credentials*"
  - "**/.env*"

git_push_remotes:
  - "origin"
```

**Hardcoded denials (never overridable):** private keys, cloud credentials, path traversal.

**Human-in-the-loop**: All host-action requests appear in TUI. Human approves/denies each. Roles define what *can* be requested; TUI gives final approval. This is the key anti-privilege-escalation mechanism.

**Per-agent overrides via CLI:**
```bash
./run-agent.sh ./project "prompt" --role code-agent --allow-path /extra/path --name my-agent
```

## Container Security (Podman)

- Rootless Podman (no daemon, no root on host)
- Non-root user `agent` (uid 1000) inside container
- `--cap-drop=ALL`, only `NET_RAW` for DNS
- `--security-opt=no-new-privileges`
- Read-only root filesystem + tmpfs for `/tmp`, caches
- Workspace at `/workspace` is the only writable mount
- No `--privileged`, no host PID/network namespace

## Container Entrypoint

```bash
#!/bin/bash
set -e
cat > /tmp/mcp-config.json <<EOF
{
  "mcpServers": {
    "agent-bridge": {
      "command": "node",
      "args": ["/opt/bridge/dist/index.js"],
      "env": {
        "ORCHESTRATOR_URL": "${ORCHESTRATOR_URL}",
        "AGENT_NAME": "${AGENT_NAME}",
        "AGENT_ROLE": "${AGENT_ROLE}"
      }
    }
  }
}
EOF

exec claude \
  --dangerously-skip-permissions \
  --mcp-config /tmp/mcp-config.json \
  -p "${AGENT_PROMPT}"
```

## CLI Flow (`run-agent.sh`)

1. Parse args (project path, prompt, --role, --name, --allow-path)
2. Build container image if needed (`podman build -f Containerfile`)
3. Create pod network if needed (`podman network create agent-net`)
4. Start orchestrator if not running (background process, PID file)
5. Launch container with workspace mount + env vars
6. Orchestrator TUI shows agent connecting and its activity
7. Cleanup on exit

## Implementation Phases

### Phase 1: Minimal loop (ask_user end-to-end)
1. Rust workspace + crate structure, Cargo.toml files
2. `core::server` -- WebSocket server with register + user_prompt
3. `tui` -- basic ratatui app showing agents + pending prompts
4. Bridge: WS client + MCP server with `ask_user` tool
5. Containerfile + entrypoint.sh
6. run-agent.sh basic flow
7. Test: agent asks question -> appears in TUI -> user answers -> flows back

### Phase 2: Host capabilities
8. `read_host_file` handler + permission checks
9. `git_push` handler
10. Role YAML loading (`serde_yaml`)
11. TUI: approve/deny UI for file_read and git_push requests
12. Hardcoded security denials

### Phase 3: Multi-agent
13. Agent registry + service discovery
14. `list_agents` and `message_agent` tools
15. Inter-agent message routing + queuing
16. TUI: multi-agent view, switch between agent outputs
17. Test with two containers

### Phase 4: Hardening
18. Full container security (read-only root, cap-drop, etc.)
19. Playwright in container verification
20. Reconnection logic, graceful error handling
21. Graceful shutdown orchestration
