# Leaky fields

**Principle:** a type meant to be generic over implementations doesn't
carry one implementation's vocabulary. If it must (for now), the leak is
named, not laundered.

## Anti-pattern

```rust
/// Request to start an agent — nominally runtime-agnostic.
pub struct StartAgentPayload {
    // ...
    /// Override Claude Code's `--model`.
    pub model: Option<String>,
    /// Override Claude Code's `--effort` (low|medium|high|xhigh|max).
    pub effort: Option<String>,
}
```

Why it's wrong: the generic launch message now speaks one CLI's flag
grammar. A second runtime either ignores the fields (silent dead config) or
reinterprets them (subtle divergence). The doc comments citing another
tool's flags are the tell — the abstraction boundary leaks its current sole
implementation.

## Correct

Give the implementation-specific values a home that *names* the
implementation. For config, the usual right answer is an enum with one
variant per implementation — specific things get specific fields, and the
generic type carries the enum:

```rust
pub enum RuntimeTuning {
    Claude { model: String, effort: Effort },
    Codex  { model: String, reasoning: ReasoningMode },
}

pub struct ResolvedLaunch {
    // ...
    pub tuning: RuntimeTuning,   // generic type, honest contents
}
```

Now the vocabulary is scoped to its variant, cross-implementation nonsense
(`effort` on a Codex launch) is unrepresentable, and adding a runtime makes
the compiler point at every site that must handle it.

Reach for a trait instead only when the set is *open* — implementations
supplied from somewhere else (another crate, plugins, users) that this code
must accept without being edited:

```rust
trait AgentRuntime {
    fn launch_args(&self, launch: &ResolvedLaunch) -> Vec<String>;
}
```

Closed set you control → enum. Open set extended by others → trait. A
trait for a closed in-repo set is indirection with no payoff: you lose
exhaustive matching and gain nothing.

## Exceptions

- **Pool of one, honestly labeled.** With exactly one implementation and no
  concrete second on the horizon, even the enum can be premature — a
  one-variant enum is ceremony. The right minimal move is to *name* the
  coupling (`// Claude Code-specific; becomes a RuntimeTuning variant when
  a second runtime exists`) so it's findable, and keep the field. The
  anti-pattern is the *unmarked* leak in a type that claims generality.
- **Deliberately non-generic types.** A struct named `ClaudeLaunchArgs` can
  say `--effort` all it wants. Leaks are only leaks across a boundary that
  promises abstraction.
- **Escape hatches.** An explicit `extra_args: Vec<String>` passthrough is
  an honest, labeled hole — acceptable at the edge, never deep inside.

## Review cues

- Doc comments on a generic type naming a specific tool's flags, env vars,
  or file paths.
- `Option` fields that only one implementation reads.
- Enums/valid-value lists in comments that mirror one vendor's CLI help.
- A "generic" interface whose methods only make sense for the current sole
  implementor.
