# Illegal states

**Principle:** if a field is only meaningful when another field has a
certain value, the field belongs inside that variant. Make the invalid
combination unconstructible instead of policing it at runtime.

## Anti-pattern

```rust
pub mode: String,            // "long-running" | "oneshot"
pub resume_session: bool,    // meaningful only when mode == "long-running"
```

Why it's wrong: `mode: "oneshot", resume_session: true` is representable but
meaningless. Every consumer must know the unwritten rule; tests must cover
combinations that shouldn't exist; a refactor that reorders checks can
activate the dead flag.

## Correct

Move the dependent data into the variant that owns it:

```rust
pub enum Mode {
    LongRunning { resume: bool },
    Oneshot,
}
```

Now "oneshot with resume" is not a bug you catch — it's a program that
doesn't compile. The general moves:

- dependent field → variant payload (as above)
- "at least one of a/b" → an enum of the three legal shapes
- "this Vec is never empty" → a `NonEmpty` type or a constructor that
  refuses empties
- paired options (`Option<A>`, `Option<B>` that must match) → one
  `Option<(A, B)>`

## Exceptions

- **External schema mirrors.** A struct that mirrors JSON/DB/FFI you don't
  control may have to represent what the wire allows; validate at the parse
  boundary into a stricter internal type instead (parse, don't validate).
- **Combinatorial explosion.** When the legal-state space is large and
  irregular, a validating constructor (`fn new(...) -> Result<Self>`) plus a
  non-public constructor path beats a 40-variant enum. The invariant still
  lives in one place — construction — not scattered through consumers.
- **Transitional states in migrations.** Temporarily representable illegal
  states during a phased refactor are fine if a later phase in the same plan
  removes them.

## Review cues

- Doc comments of the form "only used when...", "ignored unless...".
- `bool` fields adjacent to a mode/kind/type field.
- Runtime checks re-asserting the same invariant in multiple consumers.
- Test fixtures that must set a field to a "doesn't matter" value.
