# Kitchen-drawer config

**Principle:** a struct's fields should change for one reason; a bag that
accretes a field per feature is a missing set of types.

## Anti-pattern

One flat struct spanning six unrelated domains:

```rust
pub struct StartAgentPayload {
    pub name: String, pub role: String,            // identity
    pub mode: String, pub prompt: String,          // workload
    pub model: Option<String>,                     // runtime tuning
    pub effort: Option<String>,
    pub project_path: String,                      // filesystem wiring
    pub agent_dir: String,
    pub seed_credentials: String,
    pub extra_mounts: Vec<(String, String)>,
    pub image_name: String,                        // image selection
    pub network_name: String,                      // network topology
    pub orchestrator_port: u16, pub mcp_port: u16,
    pub dolt_port: Option<u16>,
}
```

Why it's wrong: every feature in any domain grows the struct; every
constructor must supply all of it (hence giant literal fixtures in tests);
`#[serde(default)]` accumulates on newer fields because no sender wants to
know about all of them. The grouping comments *are* the missing type names.

## Correct

Split along the reasons-to-change. Often the split reveals that some groups
aren't inputs at all but resolution outputs:

```rust
pub struct AgentSpec { name, role, mode, prompt }          // intent
pub struct ResolvedLaunch {                                 // derived, one builder
    pub spec: AgentSpec,
    pub mounts: Mounts,        // if the group is cohesive, name it
    pub network: NetworkPorts,
    // ...
}
```

Don't over-split either: nest a sub-struct only when the group travels
together to more than one consumer. Two levels is usually plenty.

## Exceptions

- **Edge-of-app config mirrors.** A struct that 1:1 mirrors a config file or
  CLI args at the parse boundary may legitimately be wide — provided it is
  immediately decomposed and nothing deep in the app takes the whole thing.
- **Small bags.** Five fields spanning two concerns is not a kitchen drawer.
  The smell needs both breadth (many domains) and gravity (everything keeps
  landing there).
- **Builder internals.** A private builder can hold everything mid-flight;
  the smell is about public/shared types.

## Review cues

- Field-grouping comments inside a struct (`// network stuff`).
- A diff that adds one field to a struct plus one argument to every
  constructor in the codebase.
- `#[serde(default)]` on the newest N fields, growing with each PR.
- Functions taking the big struct but reading two fields of it.
