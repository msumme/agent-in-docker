# Code Review: agent-in-docker

**Date:** 2026-04-08
**Scope:** Full codebase -- 4 Rust crates, Containerfiles, role configs
**Focus:** Testability, composability, duplication, missing features

---

## 1. Duplicated Code

### `setup_agent_dir` / `ensure_credentials` / `copy_seed_to_agent_dir` / `copy_dir_recursive` duplicated across CLI and core

These two files contain near-identical implementations:
- `ensure_credentials` (cli/src/config.rs:71 vs core/src/project_config.rs:35)
- `setup_agent_dir` (cli/src/config.rs:84 vs core/src/project_config.rs:48)
- `copy_seed_to_agent_dir` (cli/src/config.rs:115 vs core/src/project_config.rs:74)
- `copy_dir_recursive` (cli/src/config.rs:135 vs core/src/project_config.rs:93)

The CLI's `Config` even has `to_project_config()` at config.rs:56, acknowledging the overlap. The CLI should just use `ProjectConfig` from core for agent dir setup and credential checks, and keep `Config` as a thin CLI-specific layer (discovery logic, containerfile path, orchestrator binary path, PID file).

### `find_latest_backup` duplicated

Duplicated in `cli/src/login.rs:59` and `entrypoint/src/setup.rs:112` with slightly different signatures (`&Path -> Option<PathBuf>` vs `&str -> Option<String>`). This should live once in `core`.

### Auto-accept shell script duplicated

The auto-accept dialogs shell script is duplicated in `cli/src/container.rs:157-163` (inline in `launch_long_running`) and `core/src/agent_manager.rs:157-163` (inline in `start_agent`). The standalone `auto_accept_dialogs` function at container.rs:222 is dead code -- never called.

### `podman_run_args` logic duplicated

Between `cli/src/container.rs:54` (returns `Vec<String>`) and `core/src/agent_manager.rs:61` (`RealContainerOps::build_run_command`, returns a `String`). Same env vars, same volume mounts, same capabilities, assembled differently. This should be one function.

---

## 2. Testability / DI Issues

### CLI `main.rs` -- entirely untestable

The `main()` function at cli/src/main.rs:41 does all orchestration inline: config discovery, credential checks, agent dir setup, image building, network creation, service startup, and container launch. None of this is behind traits or injectable. You can't test the `Run` command flow without actually spawning containers.

### `container.rs` functions are all free functions calling `Command::new("podman")` directly

`image_exists`, `build_image`, `ensure_network`, `launch_oneshot`, `launch_long_running` -- none injectable. Compare with `core/src/agent_manager.rs` which properly uses `TmuxOps` and `ContainerOps` traits. The CLI container module should use the same trait-based approach.

### `services.rs` -- `ensure_orchestrator` and `ensure_dolt` are untestable

They call `Command::new("cargo")`, `Command::new("tmux")`, `Command::new("bd")`, and do TCP port checks, all hardcoded. The port check helper `is_port_listening` is the one piece that *is* tested.

### `entrypoint` crate -- all side effects baked in

`setup.rs` reads/writes the filesystem directly (`restore_claude_json`, `verify_credentials`, `pre_accept_workspace_trust`). `register.rs` makes real WebSocket connections. The `main()` spawns a real process. There are no traits or abstractions -- you can only test these inside an actual container. The `write_mcp_config_to` extraction (setup.rs:94) is the right pattern; the other functions need the same treatment.

### TUI `main.rs` -- hardcoded wiring

The TUI main creates `RealTmuxOps` and `RealContainerOps` directly (tui/main.rs:44-46), binds real TCP ports, and creates real terminals. The `App` struct itself is well-factored (takes `cmd_tx`, returns `KeyEffect`), but the server/MCP setup isn't injectable.

### `AgentManager::start_agent` spawns a real shell process

At agent_manager.rs:162-163, even though it takes injectable `TmuxOps`/`ContainerOps` for everything else. The auto-accept script bypass makes the whole function have a real side effect in tests (though the `FakeTmux` prevents the tmux call itself).

### `McpState` uses post-construction mutation instead of constructor injection

`McpState` uses `Mutex<Box<dyn Trait>>` for runtime swapping (mcp.rs:109-110) rather than constructing with the right dependency. `set_registry` and `set_permissions` are called post-construction in server.rs:436-439. This means there's a window where `McpState` has `NoOpRegistry` and `AllowAllPermissions` as defaults that could serve real requests. Constructor injection would be cleaner.

---

## 3. Architectural Issues

### `server::run` has 7 parameters, several `Option<Arc<...>>`

The `Option` wrapping means every use site has `if let Some(ref mgr) = agent_mgr` guards (server.rs:488, 519, 621, 668, 690, 731). This is a sign that the function is doing too much -- it's simultaneously a WS server, a TUI command processor, and an agent lifecycle manager.

### `RunConfig` (cli) vs `StartAgentPayload` (core/types) -- nearly identical structs

`RunConfig` has `agent_dir` and `seed_credentials` as `String`; `StartAgentPayload` has them too. The CLI builds a `RunConfig` to call `container::launch_*`; the TUI builds a `StartAgentPayload` to call `AgentManager::start_agent`. Same data, two structs, two code paths.

### `Config` (cli) vs `ProjectConfig` (core) -- same overlap

`Config` has extra CLI-specific fields (orchestrator_bin, containerfile, entrypoint, pid_file), but the overlap in core fields is total. The `to_project_config()` converter is evidence of the design wanting to collapse these.

### `mode` and `role` are stringly-typed

`mode` is `String` everywhere (`RunConfig`, `StartAgentPayload`, `ManagedAgent`) when it's really an enum of two values: `"oneshot"` and `"long-running"`. Same for `role` -- it's always compared against known strings but never validated.

### `server::run_with_id_gen` never returns `Ok(())`

The TCP accept loop (`loop { listener.accept()... }`) runs forever or errors. Shutdown relies on the tokio runtime dropping.

---

## 4. Missing Features for Basic Usage

### No `init` command

There's no way to set up a project for agent-in-docker usage. A user needs to manually create `.claude-container/`, seed credentials, and have the right Containerfile. An `agent init <project-path>` that scaffolds this would be the most basic onramp.

### No project path in the TUI flow

When the TUI creates a new agent (via `N` key), the `StartNewAgent` command (types.rs:174) only takes `name` and `role`. The project path comes from whatever `ProjectConfig` was passed at startup. This means you can only work on one project at a time per orchestrator instance. There's no way to specify "start an agent on *this* repo."

### No way to send an initial prompt from the TUI

The `StartNewAgent` TUI command has no `prompt` field. When the server handles it (server.rs:502), it sets `prompt: String::new()`. So agents started from the TUI launch but just sit idle at the Claude Code prompt. The CLI's `Run` command supports prompts, but the TUI doesn't expose this.

### No agent output visibility

The TUI has `OrchestratorEvent::AgentOutput` (types.rs:144) but nothing produces it. You can see agents connecting/disconnecting and pending requests, but there's no visibility into what an agent is actually doing. You have to `tmux attach` to see anything.

### No devcontainer/Containerfile awareness

Per the project direction toward project devcontainers + tools bundle, currently the system uses a single global Containerfile. There's no discovery of `.devcontainer/devcontainer.json` or per-project container configs.

### No `status` command

The CLI has `Run` and `Login`. There's no `agent status` to show what's running, or `agent stop` to stop agents, or `agent attach` to connect to one. The TUI fills this role, but you shouldn't need a TUI just to check if things are running.

### No credential refresh handling

The entrypoint copies credentials, but if the OAuth token expires during a long-running session, the agent dies. The shared bind-mount of `.credentials.json` (container.rs:72) helps, but there's no monitoring or refresh flow.

---

## 5. Dead Code

| Location | What | Action |
|----------|------|--------|
| `cli/src/container.rs:222-262` | `auto_accept_dialogs` function | Delete (duplicated as inline shell script in `launch_long_running`) |

---

## 6. What's Working Well

1. **Trait-based DI in core** -- `RequestExecutor`, `IdGenerator`, `AgentRegistry`, `PermissionCheck`, `TmuxOps`, `ContainerOps`, `EnvResolver` all follow the right pattern
2. **Test quality** -- Tests that exist use fakes, test behavior not implementation
3. **Security layering** -- Container isolation + permission checker + hardcoded denials + human-in-the-loop
4. **MCP SSE keepalive** -- Streaming handler with 15s keepalive comments for long-running approvals
5. **Event/command channel architecture** -- Clean separation between server and TUI via `OrchestratorEvent` / `TuiCommand`
6. **App state machine** -- `handle_key() -> KeyEffect` is a good pattern; `App` is testable in isolation

---

## 7. Recommendations (priority order)

1. **Consolidate duplicated code**: CLI `config.rs` should delegate to `core::project_config` for agent setup / credentials. Kill the duplicated `find_latest_backup`, `copy_seed_to_agent_dir`, etc.
2. **Unify `RunConfig` and `StartAgentPayload`** into one struct in `core::types`.
3. **Add `init` and `status` CLI commands** -- these are the minimum for a new user to get started.
4. **Add prompt support to `StartNewAgent`** so the TUI can actually task agents.
5. **Make `mode` and `role` proper enums** instead of stringly-typed.
6. **Extract the auto-accept script** into a single function/module; delete the dead `auto_accept_dialogs` function.
7. **Inject `Command` execution in the CLI** the same way core does with `TmuxOps`/`ContainerOps`, so the CLI's orchestration logic becomes testable.
