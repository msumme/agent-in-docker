# Code Review: agent-in-docker

**Date:** 2026-04-08
**Scope:** Full codebase -- 4 Rust crates, Containerfiles, role configs
**Focus:** Testability, composability, duplication, missing features

---

## 1. Duplicated Code -- RESOLVED

All duplication issues identified have been fixed:

- **`setup_agent_dir` / `ensure_credentials` / `copy_seed_to_agent_dir` / `copy_dir_recursive`**: CLI `config.rs` stripped down to CLI-specific config only. All agent dir setup and credential checks now delegate to `core::project_config`.
- **`find_latest_backup`**: Single implementation in `core::project_config::find_latest_backup()`. CLI `login.rs` uses it. Entrypoint keeps its own copy (intentionally minimal dependency set for container image).
- **`RunConfig` vs `StartAgentPayload`**: `RunConfig` deleted. CLI now uses `StartAgentPayload` from `core::types` everywhere.
- **`podman_run_args` logic**: Single implementation as `StartAgentPayload::container_run_args()` in `core::types`. Both CLI and `RealContainerOps` use it.
- **Auto-accept shell script**: Extracted to `core::agent_manager::auto_accept_script()`. Both CLI `launch_long_running` and `AgentManager::start_agent` call it. Dead `auto_accept_dialogs` function deleted.

---

## 2. Testability / DI Issues

### CLI `main.rs` -- entirely untestable

The `main()` function does all orchestration inline: config discovery, credential checks, agent dir setup, image building, network creation, service startup, and container launch. None of this is behind traits or injectable.

### `container.rs` functions are all free functions calling `Command::new("podman")` directly

`image_exists`, `build_image`, `ensure_network`, `launch_oneshot`, `launch_long_running` -- none injectable. Compare with `core/src/agent_manager.rs` which properly uses `TmuxOps` and `ContainerOps` traits. The CLI container module should use the same trait-based approach.

### `services.rs` -- `ensure_orchestrator` and `ensure_dolt` are untestable

They call `Command::new("cargo")`, `Command::new("tmux")`, `Command::new("bd")`, and do TCP port checks, all hardcoded. The port check helper `is_port_listening` is the one piece that *is* tested.

### `entrypoint` crate -- all side effects baked in

`setup.rs` reads/writes the filesystem directly (`restore_claude_json`, `verify_credentials`, `pre_accept_workspace_trust`). `register.rs` makes real WebSocket connections. The `main()` spawns a real process. There are no traits or abstractions -- you can only test these inside an actual container. The `write_mcp_config_to` extraction is the right pattern; the other functions need the same treatment.

### TUI `main.rs` -- hardcoded wiring

The TUI main creates `RealTmuxOps` and `RealContainerOps` directly, binds real TCP ports, and creates real terminals. The `App` struct itself is well-factored (takes `cmd_tx`, returns `KeyEffect`), but the server/MCP setup isn't injectable.

### `AgentManager::start_agent` spawns a real shell process

Even though it takes injectable `TmuxOps`/`ContainerOps` for everything else, the `auto_accept_script` call spawns a real `sh` process. In tests, the `FakeTmux` prevents the tmux call but this side effect leaks.

### `McpState` uses post-construction mutation instead of constructor injection

`McpState` uses `Mutex<Box<dyn Trait>>` for runtime swapping rather than constructing with the right dependency. `set_registry` and `set_permissions` are called post-construction. This means there's a window where `McpState` has `NoOpRegistry` and `AllowAllPermissions` as defaults that could serve real requests.

---

## 3. Architectural Issues

### `server::run` has 7 parameters, several `Option<Arc<...>>`

The `Option` wrapping means every use site has `if let Some(ref mgr) = agent_mgr` guards. This is a sign that the function is doing too much -- it's simultaneously a WS server, a TUI command processor, and an agent lifecycle manager.

### `Config` (cli) vs `ProjectConfig` (core) -- partial overlap remains

`Config` has CLI-specific fields (orchestrator_bin, containerfile, entrypoint, pid_file) plus duplicated core fields. The `to_project_config()` converter bridges the gap. Could be simplified by having `Config` hold a `ProjectConfig` rather than duplicating fields.

### `mode` and `role` are stringly-typed

`mode` is `String` everywhere (`StartAgentPayload`, `ManagedAgent`) when it's really an enum of two values: `"oneshot"` and `"long-running"`. Same for `role` -- it's always compared against known strings but never validated.

### `server::run_with_id_gen` never returns `Ok(())`

The TCP accept loop (`loop { listener.accept()... }`) runs forever or errors. Shutdown relies on the tokio runtime dropping.

---

## 4. Missing Features for Basic Usage

### No `init` command

There's no way to set up a project for agent-in-docker usage. A user needs to manually create `.claude-container/`, seed credentials, and have the right Containerfile. An `agent init <project-path>` that scaffolds this would be the most basic onramp.

### No project path in the TUI flow

When the TUI creates a new agent (via `N` key), the `StartNewAgent` command only takes `name` and `role`. The project path comes from whatever `ProjectConfig` was passed at startup. This means you can only work on one project at a time per orchestrator instance.

### No way to send an initial prompt from the TUI

The `StartNewAgent` TUI command has no `prompt` field. Agents started from the TUI launch but just sit idle at the Claude Code prompt. The CLI's `Run` command supports prompts, but the TUI doesn't expose this.

### No agent output visibility

The TUI has `OrchestratorEvent::AgentOutput` but nothing produces it. You can see agents connecting/disconnecting and pending requests, but there's no visibility into what an agent is actually doing. You have to `tmux attach` to see anything.

### No devcontainer/Containerfile awareness

The system uses a single global Containerfile. There's no discovery of `.devcontainer/devcontainer.json` or per-project container configs.

### No `status` command

The CLI has `Run` and `Login`. There's no `agent status` to show what's running, or `agent stop` to stop agents, or `agent attach` to connect to one. The TUI fills this role, but you shouldn't need a TUI just to check if things are running.

### No credential refresh handling

The entrypoint copies credentials, but if the OAuth token expires during a long-running session, the agent dies. The shared bind-mount of `.credentials.json` helps, but there's no monitoring or refresh flow.

---

## 5. Dead Code -- RESOLVED

The dead `auto_accept_dialogs` function in `cli/src/container.rs` has been deleted.

---

## 6. What's Working Well

1. **Trait-based DI in core** -- `RequestExecutor`, `IdGenerator`, `AgentRegistry`, `PermissionCheck`, `TmuxOps`, `ContainerOps`, `EnvResolver` all follow the right pattern
2. **Test quality** -- 103 tests, all use fakes and test behavior not implementation
3. **Security layering** -- Container isolation + permission checker + hardcoded denials + human-in-the-loop
4. **MCP SSE keepalive** -- Streaming handler with 15s keepalive comments for long-running approvals
5. **Event/command channel architecture** -- Clean separation between server and TUI via `OrchestratorEvent` / `TuiCommand`
6. **App state machine** -- `handle_key() -> KeyEffect` is a good pattern; `App` is testable in isolation
7. **Single source of truth for container config** -- `StartAgentPayload::container_run_args()` used by both CLI and TUI paths

---

## 7. Recommendations (priority order)

1. ~~Consolidate duplicated code~~ -- DONE
2. ~~Unify `RunConfig` and `StartAgentPayload`~~ -- DONE
3. **Add `init` and `status` CLI commands** -- these are the minimum for a new user to get started.
4. **Add prompt support to `StartNewAgent`** so the TUI can actually task agents.
5. **Make `mode` and `role` proper enums** instead of stringly-typed.
6. **Inject `Command` execution in the CLI** the same way core does with `TmuxOps`/`ContainerOps`, so the CLI's orchestration logic becomes testable.
7. **Compose `Config` around `ProjectConfig`** -- have CLI's `Config` hold a `ProjectConfig` field rather than duplicating its fields and converting.
