# Inject at construction, pass at call

**Principle:** every value a unit uses arrives in exactly one of two ways —
injected at construction (stable for the unit's lifetime: configuration,
dependencies, resolved paths/ports/clients) or passed as a function argument
(varies per call). Code that reaches into shared context and recomputes a
value at call time is wiring leaking into the call path.

## The classification test

For each value a unit needs:

1. Does it vary per call? → function parameter.
2. Is it fixed at startup for the program's operation? → constructor,
   wired once at the composition root.
3. Is it *derived* from those? → derive it exactly once, where its inputs
   live: lifetime-stable derivations happen at construction; per-call
   derivations happen inside the callee — never at each caller.

Clause 3 is the load-bearing one: a derivation belongs to the owner of its
inputs, so there is one site, not a copy per caller.

## Anti-pattern

Call sites holding shared config and re-performing wiring per call:

```rust
// three call sites, each doing this independently before "launch"
let payload = StartAgentPayload {
    seed_credentials: cfg.seed_dir.join(".credentials.json")
        .to_string_lossy().to_string(),
    image_name: cfg.image_name.clone(),
    network_name: cfg.network_name.clone(),
    orchestrator_port: cfg.orchestrator_port,
    // ...
};
```

Why it's wrong: the derivation is duplicated at every caller — adding a
call site copies it again, changing it means finding every copy, and one
gets missed. (It happened here: a field-removal spec listed the known
construction sites and missed a third in `server.rs`.)

## Correct

Wiring happens once, at startup; call sites pass only per-call data:

```rust
// composition root — once
let launcher = AgentLauncher::new(&cfg)?;   // resolves image, network,
                                            // ports, cred path HERE

// call sites — per-call data only
launcher.launch(AgentSpec { name, role, mode, prompt })?;
```

Callers can't get the derivation wrong because they don't do it. Adding a
call site adds zero wiring. Testing falls out: construct `AgentLauncher`
with fake wiring, exercise calls with plain per-call data.

See `patterns/app-composition.md` for the full composition-root shape.

## Exceptions

- **The composition root.** `main`/startup reads raw config and performs
  derivations — that is the wiring step's job. The rule is that nothing
  *else* does.
- **Hot-reloadable config.** If config legitimately changes at runtime, the
  "current config" source becomes an injected dependency itself (a
  provider/watcher), wired once; re-reading is explicit through it.
- **One-off derivations.** Deriving a value inline in the single place it's
  used is fine. The smell requires repetition across call sites, or a
  derivation living far from the owner of its inputs.
- **Per-call-fresh values** (now, randomness, request IDs). Passing the
  value as a plain argument (`fn expire(&mut self, now: Timestamp)`) is
  fine; obtaining it from an injected capability (clock, id-gen) is fine.
  What's wrong is a callee pulling from ambient globals
  (`SystemTime::now()`, `thread_rng()`) — or being handed the value when it
  already holds the source (then the parameter is redundant and the two can
  disagree).

## Review cues

- The same derivation expression (`cfg.x.join(...)`, `cfg.y.clone()`
  clusters) at more than one call site.
- Functions taking `&Config` but reading two or three fields of it.
- A unit whose constructor takes nothing while every method takes config.
- `SystemTime::now()` / `thread_rng()` / env reads anywhere but the
  composition root or a capability impl.
- "Can't call this without building the whole config" in tests.
