# Stringly-typed

**Principle:** a value with a closed set of legal forms is an enum; absence
is `Option`, never a sentinel value. Parse at the boundary, then carry the
parsed type.

## Anti-pattern

```rust
pub mode: String,        // compared against "long-running"/"oneshot" literals
                         // at five call sites
pub role_prompt: String, // doc comment: "Empty string means no role prompt"
```

Why it's wrong: `"long-runing"` compiles and fails at runtime, possibly
silently (a `match` with a `_` arm just takes the default). The empty-string
sentinel is an invariant only the doc comment knows; every consumer must
remember to check `.is_empty()`, and one won't.

## Correct

```rust
pub enum Mode { LongRunning { resume: bool }, Oneshot }

pub role_prompt: Option<String>,   // absence is a type, not a convention
```

Parse once where the string enters (deserialization, env var, CLI arg) and
match exhaustively everywhere else — the compiler then finds every call site
when a variant is added:

```rust
let mode = match env_mode.as_str() {
    "long-running" => Mode::LongRunning { resume },
    "oneshot" => Mode::Oneshot,
    other => bail!("unknown AGENT_MODE {other}"),
};
```

## Exceptions

- **Open sets.** If users can extend the set without touching this code, a
  string is *correct*. Example from this codebase: `role: String` — roles
  are data (a markdown file anyone can add), so an enum would wrongly close
  the set. Consider a newtype (`struct RoleName(String)`) if it travels far.
- **The boundary itself.** The line that reads the env var / JSON obviously
  handles a string. The smell is the string *escaping* the boundary
  unparsed.
- **Pass-through values.** Data this code never branches on (an opaque ID
  forwarded elsewhere) can stay a string; parsing it buys nothing.

## Review cues

- The same string literal compared in more than one file.
- Doc comments defining a sentinel ("empty means...", "-1 disables...").
- A `match x.as_str()` with a `_ =>` arm that silently defaults instead of
  erroring.
- `bool` + `String` pairs that encode what one enum would.
