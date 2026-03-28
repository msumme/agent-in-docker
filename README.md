# agent-in-docker

Run LLM code agents inside Podman containers with full internal freedom but restricted host access. The container boundary is the security model -- agents run with `--dangerously-skip-permissions` inside, while a host-side orchestrator with a TUI dashboard mediates all external actions.

## How it works

```
Host                                 Podman Container
┌──────────────────────┐            ┌─────────────────────┐
│  Orchestrator (Rust) │◄──── ws ───│  Bridge (TS/MCP)    │
│  ┌────────────────┐  │            │       │              │
│  │ TUI Dashboard  │  │            │       ▼              │
│  │ - agent list   │  │            │  Claude Code (or     │
│  │ - approve/deny │  │            │  any LLM agent)      │
│  │ - answer       │  │            │                      │
│  └────────────────┘  │            │  /workspace (mount)  │
└──────────────────────┘            └─────────────────────┘
```

1. The **orchestrator** runs on your host with a terminal dashboard
2. Your project directory is bind-mounted into the container at `/workspace`
3. Inside the container, a **bridge** process exposes MCP tools (like `ask_user`) to the agent
4. The agent calls these tools, and requests flow to the TUI where you approve/answer them

## Prerequisites

- [Podman](https://podman.io/) (rootless)
- [Rust](https://rustup.rs/) (for building the orchestrator)
- [Node.js](https://nodejs.org/) 22+ (for building the bridge)
- `ANTHROPIC_API_KEY` environment variable set

## Quick start

```bash
# Clone and build
git clone https://github.com/msumme/agent-in-docker.git
cd agent-in-docker

# Build the orchestrator
cd orchestrator && cargo build && cd ..

# Build the bridge
cd bridge && npm ci && npm run build && cd ..

# Build the container image
podman build -f Containerfile -t agent-in-docker .

# Run an agent
./run-agent.sh ./path/to/your/project "Fix the failing tests"
```

## Usage

```
./run-agent.sh <project-path> "<prompt>" [options]

Options:
  --role <role>       Agent role (default: code-agent)
  --name <name>       Agent name (default: agent-<timestamp>)
  --no-tui            Start orchestrator in background without TUI
  --build             Force rebuild of container image
```

### Examples

```bash
# Basic usage
./run-agent.sh ./my-app "Add input validation to the signup form"

# Named agent with a specific role
./run-agent.sh ./my-app "Review the auth module for security issues" \
  --role review-agent --name security-reviewer

# Multiple agents (run in separate terminals)
./run-agent.sh ./my-app "Write the feature" --name coder
./run-agent.sh ./my-app "Review the code" --name reviewer --role review-agent
```

### What happens when you run it

1. The orchestrator binary is built if needed
2. The container image is built if needed
3. A Podman network (`agent-net`) is created
4. The orchestrator TUI starts (in a tmux session if available, otherwise foreground)
5. The container launches with your project mounted at `/workspace`

### The TUI dashboard

```
┌─ Agents ──────────┬─ Activity Log ────────────────────┐
│ ● code-agent-1    │ + code-agent-1 (code-agent) conn  │
│   role: code-agent│ [code-agent-1] Q: ok? -> A: yes   │
├─ Pending Requests ┴────────────────────────────────────┤
│ > [code-agent-1] user_prompt: Should I refactor this?  │
│ ┌ Answer (Enter to submit) ──────────────────────────┐ │
│ │ yes, go ahead                                      │ │
│ └────────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────┘
```

| Key | Action |
|-----|--------|
| Tab | Switch focus between Agents and Requests panels |
| Up/Down | Navigate within a panel |
| Enter | Submit your answer to the selected request |
| Esc | Clear input text |
| q | Quit (only when no pending requests) |

## Architecture

### Orchestrator (Rust)

The host-side process. Two crates:

- **`orchestrator-core`** -- library with WebSocket server, agent registry, and message routing. Business logic is separated from the UI for testability and future alternative frontends (web UI, headless CLI).
- **`orchestrator-tui`** -- binary with ratatui terminal dashboard.

### Bridge (TypeScript)

Runs inside the container as an MCP server on stdio. Claude Code launches it as a subprocess via `--mcp-config`. The bridge connects to the orchestrator over WebSocket and translates MCP tool calls into protocol messages.

Currently exposes one tool:

| Tool | Description |
|------|-------------|
| `ask_user` | Ask the host user a question and get their answer |

More tools planned for Phase 2+: `read_host_file`, `git_push`, `list_agents`, `message_agent`.

### Protocol

JSON messages over WebSocket: `{ id, type, from, to?, payload }`

| Message | Direction | Purpose |
|---------|-----------|---------|
| `register` / `register_ack` | container <-> host | Agent registration |
| `user_prompt` / `user_prompt_response` | container <-> host | Ask user a question |

## Development

```bash
# Run Rust tests (17 tests: server state, TUI app logic, WS integration)
cd orchestrator && cargo test

# Run TypeScript tests (5 tests: WS client, ask_user tool)
cd bridge && npm test

# Run both
cd orchestrator && cargo test && cd ../bridge && npm test
```

### Dependency injection

Both the Rust and TypeScript code use constructor-injected dependencies for testing:

- **Rust**: `IdGenerator` trait (default: UUID, test: sequential) injected into `ServerState`
- **TypeScript**: `TransportFactory` (default: real WebSocket, test: `FakeTransport`) injected into `WsClient`

## Roadmap

See [PLAN.md](PLAN.md) for full details and [tickets.md](tickets.md) for tracked work.

- **Phase 1** (done): Orchestrator + bridge + ask_user flow
- **Phase 2**: Host capabilities (file read, git push) with role-based permissions
- **Phase 3**: Multi-agent support (discovery, inter-agent messaging)
- **Phase 4**: Security hardening, Playwright in-container, graceful shutdown

## Security model

The container is the security boundary. Agents have full freedom inside (file access, code execution, package installs) but cannot:

- Access host files outside the mounted workspace
- Use host credentials (SSH keys, API tokens) directly
- Escalate privileges outside the container
- Communicate with the host except through the orchestrator's permission-checked WebSocket protocol

All host actions require human approval in the TUI dashboard.
