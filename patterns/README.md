# Code patterns

Reference library for reviewers (and producers). One file per pattern. Each
file names an anti-pattern, shows it in real code, shows the correct
alternative, and — just as important — lists the exceptions where the
"anti-pattern" is actually the right call. A reviewer citing a pattern file
cites `patterns/<name>.md` in the blocker ticket alongside the file:line of
the violation.

These are judgment aids, not lint rules. The Exceptions section exists so
reviewers don't turn a heuristic into a purity crusade: if a hunk matches an
exception, it is not a finding.

## File format

```
# <Pattern name>
**Principle:** <one line>
## Anti-pattern      — real code showing the smell, and why it's wrong
## Correct           — the alternative, in code
## Exceptions        — when this shape is fine (each with the reason)
## Review cues       — what to grep/look for when hunting this
```

## Adding patterns

New patterns arrive the same way as any instruction change: a lesson
proposal (`.agents/lessons/proposed/`, `scope: tool`,
`target: patterns/<name>.md`) reviewed and folded by the human. Keep files
short — a reviewer loads these into limited context. Little examples beat
long essays.

## Index

- [inject-at-construction](inject-at-construction.md) — wiring happens once; call sites pass per-call data
- [app-composition](app-composition.md) — base example: everything injected, non-determinism behind traits, deep integration tests
- [kitchen-drawer-config](kitchen-drawer-config.md) — one struct, one domain
- [stringly-typed](stringly-typed.md) — closed sets are enums; no sentinel values
- [illegal-states](illegal-states.md) — mode-dependent fields belong in the variant
- [mechanics-on-dtos](mechanics-on-dtos.md) — transport types don't do the work
- [leaky-fields](leaky-fields.md) — generic types don't speak one implementation's language
- [literal-fixtures](literal-fixtures.md) — tests state only what they assert
