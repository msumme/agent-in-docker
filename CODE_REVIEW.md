# Code Review: agent-in-docker

**Date:** 2026-03-30
**Reviewers:** 3x Sonnet (core, CLI, TUI+entrypoint) + 1x Opus (full project)
**Scope:** Full codebase -- 4 Rust crates (~3,500 lines), shell entrypoint, Containerfiles, role configs
**Tests:** 86 (up from 18 at first review)

---

## Critical Issues (Block Testability or Correctness)

### 1. `app.rs:190` -- TUI calls `file_read::read_file` directly
The TUI `approve_request` method calls a core I/O handler directly. Business logic in the UI layer. Makes the approval path untestable without a real filesystem. The App should only emit `TuiCommand` -- never do I/O.
**Fix:** Remove the `file_read` call. Let `TuiCommand::ApproveRequest` handle execution on the server side. Use `resolve_mcp` only for the MCP oneshot channel.

### 2. `agent_manager.rs:182,204` -- `reattach_agent`/`stop_agent` bypass `ContainerOps`
These call `std::process::Command::new("podman")` directly, while the rest of the module uses the injected `ContainerOps` trait. Zero tests for either method.
**Fix:** Add `is_running(name) -> bool` and `stop(name) -> Result` to `ContainerOps`. Move the podman calls into `RealContainerOps`.

### 3. `mcp.rs:281,354` -- Agent role hardcoded as `"code-agent"`
Every MCP permission check uses `"code-agent"` regardless of actual agent role. The `X-Agent-Name` header is read but not `X-Agent-Role`.
**Fix:** Read `X-Agent-Role` from headers. Pass both name and role into the streaming handler.

### 4. `server.rs:79` -- `RealRequestExecutor` hardcoded in `ServerState::new`
The `RequestExecutor` trait exists but the constructor always creates the real one. Tests can't inject fakes for `execute_approved_request`.
**Fix:** Accept `executor: Arc<dyn RequestExecutor>` as a constructor parameter.

### 5. CLI -- Every process-spawning function is untestable
`container.rs`, `services.rs`, `login.rs` all call `Command::new("podman"/"tmux"/"cargo"/"bd")` directly. No `ProcessRunner` trait.
**Fix:** Define a `ProcessRunner` trait. Inject it into functions that spawn processes. Provide `FakeProcessRunner` for tests.

---

## Medium Issues (Correctness Risk or DI Gaps)

### 6. `tui/main.rs:142-144` -- tmux `select-window` in key handler
Direct process call inside input dispatch. Should be `TuiCommand::AttachAgent` routed through the server, like `ReattachAgent` already is.

### 7. `tui/main.rs:88-158` -- Key dispatch logic entirely untestable
The `InputMode` state machine, modal transitions, `approval_mode` computation -- none of this is reachable from tests.
**Fix:** Extract to `app.handle_key(code, modifiers) -> AppEffect` enum. Test the state machine.

### 8. `server.rs:455-462` -- Hardcoded infra config in `StartNewAgent` handler
`project_path`, `image_name`, `network_name`, ports all hardcoded. `agent_dir` is empty string.
**Fix:** Inject an `OrchestratorConfig` struct.

### 9. `register.rs:66-96` -- Reconnection delay never resets on success
After a successful reconnect, `delay` stays at whatever it was. Should reset to 1.

### 10. Duplicated connect-receive loop in `register.rs`
Lines 14-63 and 66-96 duplicate the same connect/register/listen pattern. Extract to a single `connect_and_run` helper.

### 11. Capability divergence between CLI and AgentManager
`agent_manager.rs:RealContainerOps` adds CHOWN/SETUID/SETGID/DAC_OVERRIDE/NET_RAW. `container.rs:podman_run_args` adds only NET_RAW/DAC_OVERRIDE. Different security postures.
**Fix:** Single source of truth for container capabilities.

### 12. `entrypoint/setup.rs:74` -- Hardcoded path `/tmp/mcp-config.json`
Not injectable. Tests re-implement the logic instead of calling the function.
**Fix:** Accept `out_path: &Path` parameter.

### 13. Dual-path request resolution
TUI resolves MCP requests directly via `mcp_state.resolve()` AND sends `TuiCommand` to the WS server. Both paths fire for every approval. Fragile if either changes.
**Fix:** Single resolution path (MCP resolve for HTTP clients, WS for WS clients).

---

## Dead Code

| Location | What | Action |
|----------|------|--------|
| `cli/main.rs:141-170` | `send_ws_command` function | Delete |
| `cli/main.rs:94-106` | `payload` variable | Delete |
| `cli/Cargo.toml` | `tokio-tungstenite`, `futures-util` deps | Delete |
| `cli/main.rs:43` | `#[tokio::main]` | Remove (all code is sync now) |
| `mcp.rs:264-339` | `handle_tools_call` (non-streaming) | Delete |
| `scripts/entrypoint.sh` | Bash entrypoint | Delete (Rust binary is active) |
| `mcp.rs:96-102` | `NoOpRegistry` | Require at construction |
| `mcp.rs:83-87` | `AllowAllPermissions` | Require at construction |

---

## Duplicated Code

| What | Locations | Fix |
|------|-----------|-----|
| `find_latest_backup` | `cli/login.rs:75-87`, `entrypoint/setup.rs:107-119` | Extract to shared utility |
| Container arg construction | `container.rs:53-104`, `agent_manager.rs:57-88` | Single source, shared caps list |
| Register message JSON | `register.rs:25-33`, `register.rs:74-80` | Extract `fn register_message()` |
| Connect-receive loop | `register.rs:14-63`, `register.rs:66-96` | Extract `fn connect_and_run()` |

---

## Missing Tests (Prioritized)

### High Priority
- `app.rs` -- `approve_request` file_read branch (needs DI fix first)
- `server.rs` -- `execute_approved_request` with fake executor
- `agent_manager.rs` -- `reattach_agent`, `stop_agent` (needs DI fix first)
- `tui/main.rs` -- Key dispatch state machine (needs extraction first)
- `mcp.rs` -- Permission-denied flow through SSE stream

### Medium Priority
- `server.rs` -- `SendTask`, `ReattachAgent`, `StartNewAgent` handlers
- `container.rs` -- `podman_run_args` with ANTHROPIC_API_KEY, dolt env vars, cap flags
- `services.rs` -- `ensure_orchestrator` (needs ProcessRunner trait)
- `services.rs` -- `ensure_dolt` edge cases (missing file, port 0, malformed)
- `permissions.rs` -- `load_roles_from_dir` (reads filesystem, zero tests)
- `permissions.rs` -- Symlink traversal test

### Low Priority
- `config.rs` -- `Config::discover` (needs path injection)
- `git_push.rs` -- Positive path test (needs local bare repo)
- `container.rs` -- `write_run_script` executability, prompt quoting

---

## Test Coverage by Module

| Module | Lines | Tests | Assessment |
|--------|-------|-------|-----------|
| `server.rs` | 1160 | 14 | Good (unit + WS integration) |
| `mcp.rs` | 658 | 10 | Good (unit + HTTP integration) |
| `permissions.rs` | 405 | 15 | Good (DI with FakeEnv) |
| `agent_manager.rs` | 360 | 7 | Good (DI with FakeTmux/FakeContainer) |
| `types.rs` | 282 | 9 | Good (serialization roundtrips) |
| `app.rs` | 439 | 11 | Good (pure state logic) |
| `config.rs` | 227 | 3 | Basic (setup_agent_dir only) |
| `login.rs` | 120 | 3 | Basic (find_latest_backup only) |
| `file_read.rs` | 48 | 3 | Adequate |
| `git_push.rs` | 72 | 3 | Adequate |
| `entrypoint/setup.rs` | 174 | 3 | Basic (tests re-implement logic) |
| `container.rs` | 298 | 2 | Basic (arg generation only) |
| `services.rs` | 146 | 2 | Minimal (port check only) |
| `entrypoint/register.rs` | 121 | 1 | Minimal |
| `ui.rs` | 200 | 0 | Visual -- acceptable |
| `tui/main.rs` | 178 | 0 | Glue -- acceptable if key logic extracted |
| `entrypoint/main.rs` | 61 | 0 | Glue -- acceptable |

---

## Document Staleness

### CODE_REVIEW.md (this file) -- Regenerated
Previous version described 18 tests, said server.rs/mcp.rs/app.rs had no tests. All severely outdated.

### ARCHITECTURE_DECISIONS.md -- Stale sections
- Still describes uid detection, `adduser`, privilege dropping (all removed)
- "Entrypoint fragility" known issue -- replacement is complete
- Capabilities section says `--cap-add=NET_RAW` only -- doesn't match either launch path
- Doesn't mention the Rust entrypoint binary

---

## What's Working Well

1. **Trait-based DI architecture** -- `RequestExecutor`, `IdGenerator`, `AgentRegistry`, `PermissionCheck`, `TmuxOps`, `ContainerOps`, `EnvResolver` all follow the right pattern
2. **Test quality** -- Tests that exist use fakes, test behavior not implementation
3. **Security layering** -- Container isolation + permission checker + hardcoded denials + human-in-the-loop
4. **MCP SSE keepalive** -- Streaming handler with 15s keepalive comments for long-running approvals
5. **Event/command channel architecture** -- Clean separation between server and TUI
6. **Rust entrypoint** -- Eliminated bash fragility, adds WS registration for TUI visibility

## Top 3 Structural Changes Needed

1. **Unify request resolution** -- Single path for MCP and WS request resolution. Remove `file_read` call from App.
2. **Complete DI in AgentManager** -- Add `inspect`/`stop` to `ContainerOps`. All podman calls go through the trait.
3. **Extract key dispatch from main.rs** -- `app.handle_key()` returning `AppEffect` enum. Pure logic becomes testable.
