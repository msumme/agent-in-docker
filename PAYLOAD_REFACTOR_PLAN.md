# StartAgentPayload refactor: intent vs. resolution

Target: `orchestrator/crates/core/src/types.rs` (`StartAgentPayload`) and every
site that constructs or consumes it. This document names the anti-patterns,
shows the target pattern, and gives a mechanical fix plan. It is written to be
executable by an implementer with no prior context. Follow the phases in
order; each phase must compile and pass `cargo test --workspace` before the
next begins, and each phase is one commit.

## The core principle

**Inject at construction, pass at call** (see
`patterns/inject-at-construction.md`).

Wiring — turning startup config into operational values (paths, ports,
images, credentials) — happens exactly once, at the composition root, not
at call sites. A request struct therefore contains only what varies per
call (which role, which mode, what prompt); everything derivable from
`ProjectConfig` is resolved in one owner-side function instead of being
recomputed by every caller and shipped along.

---

## Anti-patterns present today

### A. Derived data traveling as fields

Every construction site recomputes projections of `ProjectConfig` and ships
them in the message:

```rust
// server.rs ~866 — and near-identical logic in cli/main.rs and team_cmd.rs
let payload = StartAgentPayload {
    ...
    agent_dir,
    seed_credentials: cfg.seed_dir.join(".credentials.json")
        .to_string_lossy().to_string(),
    image_name: cfg.image_name.clone(),
    network_name: cfg.network_name.clone(),
    orchestrator_port: cfg.orchestrator_port,
    ...
};
```

Why it's wrong: the derivation (`seed_dir + ".credentials.json"`) is
duplicated at 3+ sites. Adding a call site means copying it again; changing
the derivation means finding every copy. (This exact failure happened: a
field-removal spec listed the known call sites and missed `server.rs`.)

### B. Kitchen-drawer struct — one bag, six domains

```rust
pub struct StartAgentPayload {
    pub name: String, pub role: String,          // identity
    pub mode: String, pub prompt: String,        // workload
    pub resume_session: bool,                    //   "
    pub model: Option<String>,                   // runtime tuning
    pub effort: Option<String>,                  //   "
    pub project_path: String, pub agent_dir: String,   // filesystem wiring
    pub seed_credentials: String,
    pub extra_mounts: Vec<(String, String)>,
    pub image_name: String,                      // image selection
    pub network_name: String,                    // network topology
    pub orchestrator_port: u16, pub mcp_port: u16,
    pub dolt_port: Option<u16>,
    pub role_prompt: String,                     // resolved prompt text
}
```

Why it's wrong: every feature in any domain grows this struct, and every
constructor must supply all of it. One reason to change per unit — this has
six.

### C. Stringly-typed mode + sentinel values

```rust
pub mode: String,                 // compared against "long-running"/"oneshot"
pub role_prompt: String,          // "Empty string means no role prompt"
```

Why it's wrong: typos in mode strings compile fine and fail at runtime;
empty-string-means-absent is an invariant the type system can't see. Use an
enum and `Option`.

### D. Field that only applies in one mode

```rust
pub resume_session: bool,   // meaningful only when mode == "long-running"
```

Why it's wrong: `mode: "oneshot", resume_session: true` is representable but
meaningless. Fold the flag into the mode enum so the illegal state cannot be
constructed.

### E. Mechanics attached to the DTO

```rust
impl StartAgentPayload {
    pub fn container_run_args(&self) -> Vec<String> { ... }  // podman -v/-e assembly
}
```

Why it's wrong: transport data and container mechanics are different
responsibilities. Practically: testing arg assembly requires constructing all
~16 fields, which is why near-identical literal fixtures exist in
`types.rs`, `agent_manager.rs`, and `container.rs`.

### F. Runtime-specific fields in a generic message

```rust
/// Override Claude Code's `--model`. ...
pub model: Option<String>,
pub effort: Option<String>,
```

Why it's wrong: Claude CLI flag names in the generic launch message. These
belong in role/runtime configuration consulted during resolution (eventually
behind the AgentRuntime seam), not in the intent message.

### G. Literal-struct fixtures in every test

```rust
// repeated with tiny variations in 3 crates
let p = StartAgentPayload { name: "x".into(), role: "r".into(), /* 14 more */ };
```

Why it's wrong: every field change touches every fixture in every crate.
Fixtures should specify only the fields under test.

---

## The target pattern

### 1. Intent: small, serializable, wire-safe

```rust
/// What a caller asks for. This — and only this — crosses the WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSpec {
    pub name: String,
    pub role: String,
    pub mode: Mode,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum Mode {
    LongRunning { resume: bool },
    Oneshot,
}
```

`Mode::Oneshot` cannot carry a resume flag: the illegal state is gone (fixes
C and D).

### 2. Resolution: one function per launch context, orchestrator/CLI-side

```rust
/// Everything needed to actually run a container. Never crosses the wire.
#[derive(Debug, Clone)]
pub struct ResolvedLaunch {
    pub spec: AgentSpec,
    pub role_prompt: String,
    pub project_path: String,
    pub agent_dir: String,
    pub seed_credentials: String,
    pub image_name: String,
    pub network_name: String,
    pub orchestrator_port: u16,
    pub mcp_port: u16,
    pub dolt_port: Option<u16>,
    pub extra_mounts: Vec<(String, String)>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// Solo agents: derive everything from ProjectConfig. THE single place
/// where config becomes a launch.
pub fn resolve_solo_launch(cfg: &ProjectConfig, spec: AgentSpec) -> Result<ResolvedLaunch>;

/// Team agents differ legitimately (clone path as workspace, team state dir,
/// per-role model). A second named resolver — NOT optional override fields
/// on a shared context struct (that would rebuild the kitchen drawer).
pub fn resolve_team_launch(
    cfg: &ProjectConfig, team: &Team, role: &str, spec: AgentSpec,
) -> Result<ResolvedLaunch>;
```

(fixes A and B: derivation exists once per context; the wire message is
five fields).

### 3. Mechanics: free function over the resolved value

```rust
pub fn container_run_args(launch: &ResolvedLaunch) -> Vec<String> { ... }
```

(fixes E; `TmuxOps::build_run_command` and `container.rs` launchers take
`&ResolvedLaunch`).

### 4. Fixtures: a per-test-module default + struct-update syntax

```rust
#[cfg(test)]
mod tests {
    fn test_launch_default() -> ResolvedLaunch { ... }  // filler lives WITH the tests

    // in each test — only the fields under test:
    let launch = ResolvedLaunch {
        agent_dir: "/tmp/agent".into(),
        ..test_launch_default()
    };
}
```

(fixes G. The fixture default is test data owned by the test module — NOT
`impl Default` (production API; test placeholders must not be constructible
from production code) and NOT a `#[cfg(test)] impl` on the type (test
concerns don't hang off the production type's namespace).)

`model`/`effort` stay on `ResolvedLaunch` for now — they are resolution
*outputs* (from `role_model_effort`). Moving them into `roles/*.yml` and an
AgentRuntime trait is a separate, explicitly-deferred change (F is only
half-fixed here; do not attempt the trait in this refactor).

---

## Mechanical fix plan

Ground rules for every phase: no drive-by refactors, no renames beyond what
is listed, match surrounding style, run `cargo build --workspace &&
cargo test --workspace` from `orchestrator/` before committing. If a test
fails for an unrelated reason, stop and report — do not fix it.

### Phase 1 — `Mode` enum (kills C and D)

1. In `core/src/types.rs`, add the `Mode` enum (sample above) and change
   `StartAgentPayload.mode` from `String` to `Mode`; delete
   `resume_session`, folding it into `Mode::LongRunning { resume }`.
2. Grep for every read of `.mode` and `resume_session` (entrypoint env-var
   assembly in `types.rs`, `container.rs` launchers, `team_cmd.rs`
   resume-policy code, `server.rs`). Replace string comparisons with `match`.
   The container env vars (`AGENT_MODE`, `AGENT_RESUME`) keep their current
   string values — map from the enum at the env-assembly point only.
3. Fix test fixtures. The raw-JSON test at `types.rs` (~507) now asserts the
   serde shape of `Mode` instead of `resume_session`-defaults-to-false;
   update the JSON literal accordingly.
4. Wire compatibility: the WS `start_agent` path (`server.rs:1073`) now
   requires the new JSON shape. CLI and orchestrator ship from one workspace,
   so no cross-version tolerance is needed — do not add serde defaults for it.

### Phase 2 — split intent from resolution (kills A and B)

1. Add `AgentSpec` and `ResolvedLaunch` to `core/src/types.rs` (or a new
   `core/src/launch.rs` if `types.rs` is crowded — implementer's choice, one
   file only).
2. Write `resolve_solo_launch` in `core/src/project_config.rs` by extracting
   the field-assembly logic currently at `cli/src/main.rs` (~240-280) and
   `core/src/server.rs` (~840-880) — they are near-duplicates; unify them.
   Write `resolve_team_launch` by extracting the logic in
   `cli/src/team_cmd.rs` (`build_payload_for_team_agent` plus its inputs:
   clone path, team agent dir, `role_model_effort`, extra mounts).
3. Change the four construction sites to build an `AgentSpec` and call the
   appropriate resolver. The WS `start_agent` message payload
   (`server.rs:1073`) becomes `AgentSpec`; the orchestrator resolves after
   deserializing, same as its TUI path.
4. Delete `StartAgentPayload`. Consumers (`container.rs` launchers,
   `agent_manager.rs` `start_agent`/`build_run_command`) take
   `&ResolvedLaunch`.
5. `container_run_args` moves to a free function taking `&ResolvedLaunch`
   (same file as `ResolvedLaunch`). Body unchanged except field access via
   `launch.spec.*` where applicable.

### Phase 3 — fixture cleanup (kills G)

1. In each test module that builds launches (`core/src/types.rs` tests,
   `core/src/agent_manager.rs` tests, `cli/src/container.rs` tests), add
   one fixture default — `fn test_launch_default() -> ResolvedLaunch` with
   benign placeholders (empty strings, port 0, `None`, `Mode::Oneshot`
   inside the spec). The fixture lives in the test module it serves; do
   not add any method or `Default` impl to the production type, and do not
   build cross-crate sharing machinery for it. One small fn per test
   module is the correct amount of duplication: a field change then
   touches three fixture fns, not every literal in every test.
2. Rewrite the literal fixtures in those test modules to
   `..test_launch_default()` struct-update form, keeping only fields each
   test asserts on.

### Acceptance criteria

- `AgentSpec` has exactly: name, role, mode, prompt. Nothing path-, port-,
  image-, or model-shaped.
- `grep -rn "seed_dir.join" orchestrator/crates` → exactly one hit outside
  tests (inside resolution).
- No `mode == "` string comparison anywhere; no `resume_session` anywhere.
- `container_run_args` is not a method on a serializable struct.
- Every literal test fixture with more than ~5 fields is gone.
- `cargo test --workspace` green after each phase, not just at the end.

### Out of scope (do not do)

- AgentRuntime trait / moving model+effort into `roles/*.yml`.
- Changing env-var names or the run-script format the entrypoint reads.
- Renaming WS message types beyond the `start_agent` payload shape.
- Any change to team lifecycle, manifests, or clone handling.
