# Phase 1: Minimal loop (ask_user end-to-end)

## [epic] Phase 1: Minimal ask_user loop
Set up the foundational infrastructure to get a single agent running in a Podman container that can ask the user a question via the host TUI and receive an answer. This is the critical path -- every subsequent phase builds on this working loop. The architecture has three pieces: a Rust orchestrator on the host (WebSocket server + ratatui TUI), a TypeScript MCP bridge inside the container, and a shell script (`run-agent.sh`) that wires them together via Podman.

### [task] Rust workspace + crate structure
Create the orchestrator Rust workspace at `orchestrator/` with two crates: `core` (library) and `tui` (binary). The `core` crate is a library that owns all business logic: WebSocket server, message types, agent registry, permission checking, message routing. The `tui` crate is a binary that depends on `core` and provides the ratatui terminal dashboard. This separation exists so future frontends (web UI, headless CLI) can reuse `core` without the TUI.

Directory: `orchestrator/Cargo.toml` (workspace), `orchestrator/crates/core/`, `orchestrator/crates/tui/`.

Key dependencies for core: `tokio` (async runtime), `tokio-tungstenite` (WebSocket), `serde` + `serde_json` (message serialization), `uuid` (message IDs).
Key dependencies for tui: `ratatui`, `crossterm` (terminal backend), plus `core` as a path dependency.

### [task] Core WebSocket server with register + user_prompt
Implement the WebSocket server in `orchestrator/crates/core/src/server.rs`. It listens on `0.0.0.0:9800` using tokio-tungstenite. When a container connects, it waits for a `register` message containing `{ name, role }`, assigns an agent ID, stores the connection in the agent registry, and responds with `register_ack { agentId, peers: [] }`.

When a `user_prompt { question }` message arrives from an agent, the server emits a `RequestReceived` event via a `tokio::sync::mpsc` channel that the TUI subscribes to. The server holds the WebSocket send half in a map keyed by agent ID so the TUI can send back the `user_prompt_response { answer }` when the user types their reply.

Message types go in `core/src/types.rs`:
```rust
struct Message { id: String, msg_type: String, from: String, to: Option<String>, payload: Value }
```

Events emitted to TUI via channel:
```rust
enum OrchestratorEvent { AgentConnected { id, name, role }, AgentDisconnected { id }, RequestReceived { agent_id, request_id, request_type, payload } }
```

Commands received from TUI:
```rust
enum TuiCommand { RespondToRequest { request_id, payload: Value } }
```

### [task] Basic ratatui TUI showing agents + pending prompts
Implement the TUI binary in `orchestrator/crates/tui/`. The main loop uses crossterm events (keyboard input) merged with orchestrator events (from the mpsc channel) in a tokio select.

Layout (two panels, vertical split):
- **Left panel (Agents)**: List of connected agents showing name, role, and status (connected/working). Updates live as agents register/disconnect.
- **Bottom panel (Pending Requests)**: Shows user_prompt questions from agents. The currently selected request has a text input field where the user types their answer. Press Enter to submit, which sends a `TuiCommand::RespondToRequest` back to the core.

Keyboard: Tab switches focus between panels. Up/Down navigates within panels. Enter submits the current prompt answer. 'q' quits.

The TUI starts the core server in a background tokio task and communicates via channels. The TUI binary is the single entry point -- it starts everything.

### [task] TypeScript bridge: WS client + MCP server with ask_user
Create the `bridge/` TypeScript package. This runs inside the container and has two responsibilities: (1) maintain a WebSocket connection to the host orchestrator, (2) expose MCP tools to the agent via stdio.

`bridge/src/ws-client.ts`: Connects to `$ORCHESTRATOR_URL` (e.g., `ws://host.docker.internal:9800`). On connect, sends `register { name: $AGENT_NAME, role: $AGENT_ROLE }`. Waits for `register_ack`. Maintains a map of pending request IDs to Promise resolvers for request/response correlation.

`bridge/src/mcp-server.ts`: Uses `@modelcontextprotocol/sdk` with stdio transport. Registers an `ask_user` tool:
- Input schema: `{ question: string }`
- Handler: generates a UUID, sends `user_prompt { question }` via WS client, awaits the response Promise, returns `{ answer }`.

`bridge/src/index.ts`: Entry point. Connects WS client, starts MCP server on stdio. The MCP server's stdout/stdin are used by Claude Code (the agent) to call tools.

Dependencies: `@modelcontextprotocol/sdk`, `ws`, `uuid`.

### [task] Containerfile + entrypoint.sh
Create `Containerfile` (OCI format for Podman):

**Stage 1 (builder)**: FROM node:22-slim. Copy bridge/ source. Run `npm ci && npm run build`. Output: compiled JS in dist/.

**Stage 2 (runtime)**: FROM node:22-slim. Install Claude Code CLI globally (`npm install -g @anthropic-ai/claude-code`). Create non-root user `agent` (uid 1000). Copy compiled bridge from stage 1 to `/opt/bridge/`. Copy `scripts/entrypoint.sh` to `/opt/entrypoint.sh`. Set WORKDIR to `/workspace`.

`scripts/entrypoint.sh`: Generates `/tmp/mcp-config.json` pointing to the bridge (`node /opt/bridge/dist/index.js` with env vars ORCHESTRATOR_URL, AGENT_NAME, AGENT_ROLE). Then execs: `claude --dangerously-skip-permissions --mcp-config /tmp/mcp-config.json -p "$AGENT_PROMPT"`.

The key insight: Claude Code's `--mcp-config` flag tells it to launch the bridge as a subprocess via stdio. The bridge internally manages its own WebSocket connection to the orchestrator. Claude Code just sees MCP tools.

### [task] run-agent.sh basic flow
Create `run-agent.sh` as the user-facing CLI. Usage: `./run-agent.sh <project-path> "<prompt>" [--role <role>] [--name <name>]`

Flow:
1. Parse arguments. Validate project path exists. Defaults: role=code-agent, name=agent-$(date +%s).
2. Check if orchestrator is running (test TCP port 9800 or check PID file at `/tmp/agent-in-docker-orchestrator.pid`). If not, build it (`cd orchestrator && cargo build --release`) and launch in background. The orchestrator binary is the TUI, so it needs a terminal -- launch it in the current terminal and run the container in background, OR launch the TUI in a tmux pane. For Phase 1, simplest approach: the orchestrator runs in the foreground in the current terminal, and the container runs detached. The script prints the container ID so the user can `podman logs -f` it.
3. Build container image if needed: `podman build -f Containerfile -t agent-in-docker .`
4. Create network if needed: `podman network create agent-net 2>/dev/null || true`
5. Launch container: `podman run --rm --name $AGENT_NAME --network agent-net -v "$PROJECT_PATH:/workspace" -e ORCHESTRATOR_URL=ws://host.docker.internal:9800 -e AGENT_NAME=$AGENT_NAME -e AGENT_ROLE=$ROLE -e AGENT_PROMPT="$PROMPT" -e ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY agent-in-docker`
6. Trap SIGINT/SIGTERM to clean up (stop container, optionally stop orchestrator).

### [task] End-to-end test: agent asks question, user answers
Write a test procedure and a simple test project. Create `test-project/` with a few dummy files. Run: `./run-agent.sh ./test-project "List the files in this directory. Before doing anything else, use the ask_user tool to ask me what I think of the project structure."` Verify: agent connects (shows in TUI), agent calls ask_user, prompt appears in TUI pending requests, user types answer, answer returns to agent, agent continues. Document expected behavior and troubleshooting steps.

---

# Phase 2: Host capabilities

## [epic] Phase 2: Host capabilities + permissions
Add the ability for containerized agents to read specific host files and push git commits, with role-based permission checking and human-in-the-loop approval in the TUI. This is what makes the container useful beyond just running code -- agents can access host context (gitconfig, ssh config) and push their work.

### [task] read_host_file handler + MCP tool + permission checks
**Orchestrator side** (`core/src/handlers/file_read.rs`): When a `file_read { path }` message arrives, resolve the path (expand `~`, resolve `..`), check against the agent's role permissions (allowed paths, denied paths, hardcoded denials), then emit a `RequestReceived` event to the TUI for human approval. On approval, read the file and send `file_read_response { content }`. On denial, send `error { code: "PERMISSION_DENIED" }`.

**Bridge side** (`bridge/src/tools/read-host-file.ts`): MCP tool `read_host_file` with input `{ path: string }`. Sends `file_read` message via WS, awaits response. Returns file content or error message.

Path security: resolve symlinks, reject paths containing `..` after resolution, check against deny list first (always wins), then check against allow list. Paths not in any allow list are denied by default.

### [task] git_push handler + MCP tool
**Orchestrator side** (`core/src/handlers/git_push.rs`): When a `git_push { remote, branch }` message arrives, validate the remote against the role's allowed remotes. Emit to TUI for approval. On approval, execute `git -C <workspace-host-path> push <remote> <branch>` using the host's SSH agent and git credentials. The workspace host path is derived from the agent's registered workspace mount. Send `git_push_response { success, output }`.

**Bridge side** (`bridge/src/tools/git-push.ts`): MCP tool `git_push` with input `{ remote?: string, branch?: string }` (defaults: "origin", current branch). Sends message, awaits response.

Important: the git push runs on the HOST side, not inside the container. This is because the host has SSH keys and git credentials. The container's workspace is a bind mount, so the host can access the same files.

### [task] Role YAML loading + permission enforcement
Implement `core/src/permissions.rs`:
- Load role YAML files from `roles/` directory using `serde_yaml`.
- Role struct: `{ name, capabilities: HashMap<String, bool>, file_read_paths: Vec<String>, file_read_deny_paths: Vec<String>, git_push_remotes: Vec<String> }`.
- `check_permission(role: &Role, request: &Message) -> PermissionResult` -- returns Allow, Deny(reason), or NeedsApproval.
- Environment variable interpolation in paths: `${HOME}` -> actual home dir.
- Glob matching for paths using the `glob` crate.

Hardcoded denials (in code, not configurable):
- `~/.ssh/id_*` (private keys)
- `~/.aws/credentials`
- `~/.config/gcloud/application_default_credentials.json`
- Any file matching `*.pem`, `*_rsa`, `*.key` in sensitive directories

### [task] TUI approve/deny UI for host action requests
Update the TUI's pending requests panel to handle file_read and git_push requests alongside user_prompt. Each request shows: agent name, request type, details (file path or remote/branch), and [Y]/[N] indicators. Navigation: up/down to select a request, 'y' to approve, 'n' to deny. Approved requests turn green briefly, denied turn red. The panel shows a scrollable log of past decisions.

For user_prompt requests, the existing text input behavior stays. For file_read/git_push, it's a simple y/n binary choice.

### [task] Role definitions for code-agent and review-agent
Create `roles/code-agent.yml`: Full dev capabilities. Can read common config files (~/.gitconfig, ~/.ssh/config, /etc/hosts), push to origin, ask user questions, discover and message agents. Deny list includes all credential/key patterns.

Create `roles/review-agent.yml`: Read-only agent. Can read host files (same paths as code-agent), ask user questions, discover agents. CANNOT push git, CANNOT message other agents (prevents a review agent from telling a code agent to do something malicious).

---

# Phase 3: Multi-agent

## [epic] Phase 3: Multi-agent support
Enable multiple agents to run simultaneously in separate containers, discover each other via the orchestrator, and exchange messages. This enables workflows like: a code agent writes code, a review agent reviews it, they coordinate via messaging.

### [task] Agent registry + service discovery in core
Implement `core/src/registry.rs`: An in-memory registry of connected agents. Stores: agent ID, name, role, WebSocket sender handle, connection timestamp, status.

When an agent connects and registers, add to registry and broadcast `peer_joined { id, name, role }` to all other connected agents. When an agent disconnects (WebSocket close), remove from registry and broadcast `peer_left { id }`.

Handle `discover {}` messages by returning `discover_response { agents: [{ id, name, role }] }` with all currently connected agents (excluding the requester).

### [task] list_agents and message_agent MCP tools
**Bridge `list_agents` tool**: No input required. Sends `discover` message to orchestrator, returns the list of connected agents with their IDs, names, and roles. The agent can use this to find peers to collaborate with.

**Bridge `message_agent` tool**: Input `{ agentId: string, message: string }`. Sends `agent_message { to: agentId, content: message }` to orchestrator. The orchestrator routes it to the target agent. Returns delivery confirmation. Note: this is fire-and-forget for Phase 3 -- the sending agent gets confirmation of delivery but not a response. A response would require the target agent to independently call message_agent back.

### [task] Inter-agent message routing + queuing
Implement message routing in `core/src/router.rs`:
1. Receive `agent_message { to, content }` from sender.
2. Permission check: does sender's role have `message_agents: true`? Is the target agent's role in sender's `message_agents_roles` list?
3. Look up target agent in registry.
4. If target is connected, forward as `agent_message_delivery { from: sender_id, content }` to target's WebSocket.
5. If target is not connected, return `error { code: "AGENT_NOT_FOUND" }`.
6. Send `agent_message_ack { originalId, delivered: true/false }` back to sender.

On the bridge side, when an `agent_message_delivery` is received, queue it. Expose a `check_messages` MCP tool that returns all queued messages. This avoids interrupting the agent mid-task.

### [task] TUI multi-agent view
Update the TUI to handle multiple agents:
- **Agent list panel**: Shows all connected agents. Selected agent is highlighted. Press Enter or arrow keys to select.
- **Agent output panel**: Shows output/activity for the currently selected agent. Each agent has its own output buffer.
- **Pending requests panel**: Shows requests from ALL agents, prefixed with agent name. Can filter by selected agent.
- Status bar at the bottom shows: total agents, total pending requests, selected agent name.

### [task] Multi-agent integration test
Test procedure: Launch two agents targeting the same workspace (or different workspaces).
1. `./run-agent.sh ./project "You are a code agent. Use list_agents to find peers." --name code-1 --role code-agent &`
2. `./run-agent.sh ./project "You are a review agent. Use list_agents to find peers." --name review-1 --role review-agent &`
3. Verify both appear in TUI.
4. Verify each can see the other via list_agents.
5. Have code-1 send a message to review-1 via message_agent.
6. Verify review-1 receives it via check_messages.

---

# Phase 4: Hardening

## [epic] Phase 4: Hardening + security
Lock down container security, add browser automation, improve reliability and graceful shutdown. This phase makes the system production-ready.

### [task] Container security hardening
Apply full Podman security settings. Update Containerfile and run-agent.sh:
- `--cap-drop=ALL --cap-add=NET_RAW` (only DNS resolution)
- `--security-opt=no-new-privileges` (prevent SUID escalation)
- `--read-only` root filesystem with tmpfs for `/tmp`, `/home/agent/.cache`, `/home/agent/.config`, `/home/agent/.local`
- Non-root user `agent` (uid 1000) -- already in Containerfile
- No `--privileged`, no `--pid=host`, no `--net=host`
- Workspace mount is the ONLY writable bind mount
- Verify: `podman exec <container> whoami` returns `agent`, attempts to write outside /workspace and tmpfs fail, attempts to access host filesystem fail.

### [task] Playwright in-container verification
Ensure Playwright + Chromium works inside the container. Update Containerfile to install Playwright deps: `npx playwright install --with-deps chromium`. Test that an agent can launch a headless browser, navigate to a URL, and extract page content. This gives agents browser automation capability within their sandbox without any host browser access.

### [task] Reconnection logic + error handling
**Bridge reconnection**: If the WebSocket connection to the orchestrator drops, retry with exponential backoff (1s, 2s, 4s, 8s, max 30s). Re-register on reconnect. If reconnection fails after N attempts, exit with error (agent process will also exit).

**Orchestrator resilience**: If an agent's WebSocket drops unexpectedly, clean up the registry, broadcast peer_left, and reject any pending requests from that agent with an error.

**TUI error display**: Show connection errors, agent crashes, and permission denials in a dedicated error/notification area. Don't crash the TUI on individual agent failures.

### [task] Graceful shutdown orchestration
**run-agent.sh cleanup**: Trap SIGINT/SIGTERM. Send SIGTERM to the container (`podman stop`), wait up to 30s for graceful shutdown. If this was the last agent, stop the orchestrator process (kill PID from PID file). Clean up the Podman network if no containers are using it.

**TUI shutdown**: 'q' key initiates shutdown. Send disconnect notices to all agents. Wait for agents to finish current operations (with timeout). Clean exit with terminal restoration (crossterm must restore terminal state).

**Container shutdown**: SIGTERM to entrypoint -> SIGTERM to claude process -> bridge detects agent exit -> bridge sends unregister to orchestrator -> bridge exits -> container stops.
