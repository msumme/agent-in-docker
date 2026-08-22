# Mechanics on DTOs

**Principle:** a type whose job is to carry data across a boundary
(serialize, persist, transport) does not also do the work. Behavior lives
with whoever owns the mechanics.

## Anti-pattern

```rust
#[derive(Serialize, Deserialize)]
pub struct StartAgentPayload { /* 16 fields */ }

impl StartAgentPayload {
    /// Generate podman run arguments for this agent configuration.
    pub fn container_run_args(&self) -> Vec<String> { /* -v/-e assembly */ }
}
```

Why it's wrong: the transport shape and the container mechanics now change
together — a wire-format tweak risks the podman invocation and vice versa.
Practically, every test of the mechanics must construct the full transport
struct (hence 16-field literal fixtures duplicated across three crates), and
the mechanics can't evolve richer inputs without growing the wire format.

## Correct

The DTO stays inert; mechanics take it (or better, the resolved domain type)
as input:

```rust
pub fn container_run_args(launch: &ResolvedLaunch) -> Vec<String> { ... }
```

Placement rule: put the function in the module that owns the *output*
domain (container/podman concerns), not the one that owns the input type.

## Exceptions

- **Self-formatting.** `Display`/`Debug` impls, `to_string`-style rendering
  of the type's own data with no external policy — fine on any type.
- **Conversions.** `From`/`TryFrom` between representations is what DTOs are
  for; parse-and-validate constructors (`fn parse(s: &str) -> Result<Self>`)
  belong on the type.
- **Invariant-preserving accessors.** Small methods that keep a field pair
  consistent are the type's own business.
- The line is: does the method encode *another subsystem's policy* (CLI
  flags, SQL, HTML, shell)? Then it doesn't belong on the carrier.

## Review cues

- `impl` blocks on `#[derive(Serialize)]` types containing string-assembly
  for another tool (shell args, SQL, URLs).
- Tests that build large transport structs just to call one method.
- A method on a DTO whose name references a technology the DTO otherwise
  knows nothing about (`container_*`, `sql_*`, `html_*`).
