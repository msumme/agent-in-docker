# Formulas as the team-shape declaration

## Why

Epic F (Teams) currently encodes the planner → producer → reviewer → human
pipeline as imperative Rust in `TeamManager`. Beads ships `bd formula`,
which models exactly this: a DAG of typed steps with per-step assignees,
gates, variables, and composition. Moving the shape into a formula keeps
the orchestrator focused on what only it can do (spawn fresh containers,
wire MCP, manage tmux/worktrees) and gets us role-purity by construction:
each step is a separate bd issue with a separate assignee, so "fresh
container per step" is the natural unit of work.

## Scope

In: replace the hardcoded three-role lifecycle in Epic F with
`team.formula.toml` + a thin pour command. Keep all spawn/suspend/wake
mechanics in `TeamManager`.

Out: patrols, swarms, multi-PR epics. Those become follow-on formulas
once the basic shape works.

## The formula (`.beads/formulas/team.formula.toml`)

```
formula = "team"
version = 1
type    = "workflow"

[[vars]]
name = "ticket"     # parent bd id, e.g. bd-42

[[steps]]
id = "plan"
title = "Spec {{ticket}}"
assignee = "planner"

[[steps]]
id = "implement"
title = "Implement {{ticket}}"
assignee = "producer"
needs = ["plan"]

[[steps]]
id = "review"
title = "Review {{ticket}}"
assignee = "reviewer"
needs = ["implement"]
gate = { type = "gh:pr" }      # closes on PR merge/close

[[steps]]
id = "merge"
title = "Human merge {{ticket}}"
assignee = "human"
needs = ["review"]
gate = { type = "human" }
```

Rejection loop: reviewer reopens the `implement` step (or files a child
bug blocking it). Dispatcher sees `implement` ready again and spawns a
**new** producer container — fresh context, no producer-prejudice.

## Orchestrator changes (small)

1. `TeamManager::spawn(ticket)` → `bd mol pour team --var ticket=<id>`
   instead of creating three issues by hand.
2. **Dispatcher loop** (already conceptual in Epic F): poll
   `bd query mol_type=swarm AND status=open AND deps-met`; for each
   ready issue, look up the role from `assignee`, spawn a fresh
   container with that role's prompt, hand it the issue id, exit when
   the issue closes or its gate resolves.
3. Wake watcher (Epic C → F.2) closes `gh:pr` / `human` gates via
   `bd gate resolve` instead of poking team state directly.
4. New MCP tool `bd_pour_team(ticket)` (Epic E gateway) — privileged,
   host-side, so only humans/planner can launch a team.

Worktrees, suspend/resume, manifest, wake watcher: unchanged.

## bd ticket updates

- **Epic F**: rewrite F.1/F.2 to be "dispatcher + formula" instead of
  hardcoded role lifecycle. Add child ticket: "author team.formula.toml
  and seed via `bd mol seed`."
- **Epic E**: add `bd_pour_team` and `bd_resolve_gate` to the MCP
  gateway tool list.
- **Epic C**: PR-status watcher should emit `bd gate resolve` on the
  matching `gh:pr` gate, not a custom `team.wake` signal.
- **New bd-* chore**: `bd formula seed` runs in container build so
  every project picks up `team.formula.toml`.
- **Close as obsolete**: any tickets that hardcode the three-role
  graph in Rust (e.g. "TeamManager owns role list" sub-tasks).

## Verification

1. `bd cook team.formula.toml --var ticket=bd-99 --dry-run` lists 4 steps.
2. `bd mol pour team --var ticket=bd-99` creates 4 child issues with
   correct deps and assignees.
3. Spawn dispatcher against a test ticket; observe planner container
   start, exit on close; producer container start fresh; reviewer
   container start fresh; `gh:pr` gate auto-closes on merge.
4. Force-reject from reviewer (reopen `implement`); confirm a *new*
   producer container is spawned with empty context.

## Risk / unknowns

- `bd gate` Phase support: confirm `gh:pr` and `human` gates work in our
  installed bd version before depending on them.
- Formula composition (`extends`) maturity: don't rely on it for v1; one
  flat formula is enough.
- bd schema error seen today (`column "crystallizes" could not be found`)
  — needs a migration/repair pass before this lands.
