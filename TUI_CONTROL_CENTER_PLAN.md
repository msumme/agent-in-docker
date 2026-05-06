# TUI Control Center — Plan

## Goal

Turn the orchestrator TUI from a passive viewer into a working control center.
Today the TUI lists agents and surfaces pending requests; every meaningful
action (spawn, dispatch, address review, merge) happens in some other terminal.
The control center makes the TUI the single place a human operates from.

## Scope

A tabbed interface (`F1`–`F5`) replacing the current single-screen TUI:

1. **Dashboard** — AI summary header (toggle to raw event tail), agent list
   with context bars, "PRs ready for you" sidebar.
2. **Events** — full raw event stream, searchable, filterable, pausable.
3. **Commits** — real-time commit log across `main` and all agent branches,
   attributed to the agent who authored.
4. **PRs** — open PRs with review/CI/merge status, filtered to "ready for
   human" by default.
5. **Agents** — per-agent detail: PID, container, branch, current bd claim,
   context %, kill/restart/clear-context controls.

Architectural rule: the TUI is a **thin renderer** over orchestrator state.
No business logic in the TUI — selectors over a store, dispatched commands.
Summarization, gh polling, and context tracking live in the orchestrator (or
sidecar tasks); the TUI subscribes to their results.

## Mocks

### Tab 1 — Dashboard (default, summary view)

```
┌─[F1]Dashboard─[F2]Events─[F3]Commits─[F4]PRs─[F5]Agents──────────┬─merge:max─┐
│ Summary (auto · refreshed 1m12s ago · n=narrator)            [t]oggle→raw    │
│ ──────────────────────────────────────────────────────────────────────────── │
│ Last hour: max landed bd-42 (msg-fix) and opened PR #91 — green, awaiting    │
│ human merge. Bob filed bd-58 (synthetic mode hardcoded) blocking bd-42's     │
│ close. Cleaner filed bd-59 (drive-by Containerfile rewrite). Reviewer ran a  │
│ full pass on bd-42, no APPROVE yet. Merge-slot held by max for 4m.           │
│                                                                              │
├─Agents─────────────────────────┬─PRs ready for you────────────────────────── │
│ ●max      producer  ░░░░░░ 12% │ #91 bd-42 msg-fix (max)    ✓✓✓  →ready     │
│   bd-42 in_progress 2m         │ #88 bd-31 role-prompts(bo) ✓✓✓  →ready     │
│ ●bob      architect ▓▓░░░░ 38% │ #93 bd-58 synth-mode (max) ✓·✓  awaits CI  │
│   reviewing bd-42  47s         │                                             │
│ ●cleaner  cleaner   ▓▓▓░░░ 51% │ Filters: [r]eady [a]ll [m]ine               │
│   idle              5m         │                                             │
│ ●reviewer rev-agent ▓▓▓▓▓░ 84% │                                             │
│   ! near limit     12m         │                                             │
└────────────────────────────────┴───────────────────────────────────────────── ┘
 N:new agent  k:kill agent  R:restart  Enter:open PR  s:standup  q:quit
```

### Tab 2 — Events (raw)

```
┌─[F1]Dashboard─[F2]Events─[F3]Commits─[F4]PRs─[F5]Agents──────────────────────┐
│ Events (raw · 247 since 14:35 · /search)              filter:[all]  [paused] │
│ ──────────────────────────────────────────────────────────────────────────── │
│ 14:51:08 max      claim       bd-42                                          │
│ 14:51:09 max      branch      max/bd-42                                      │
│ 14:51:43 max      commit      a3f8e9 "bd-42: resolve agent ref by name|id"   │
│ 14:51:51 max      commit      b1c220 "bd-42: regression test for cli-spawn"  │
│ 14:52:04 max      push        max/bd-42 → origin                             │
│ 14:52:05 system   pr.opened   #91 max/bd-42                                  │
│ 14:52:11 max      ms.acquire  merge-slot                                     │
│ 14:52:33 max      message     → bob,cleaner,reviewer "bd-42 ready, sha a3f8" │
│ 14:53:01 reviewer claim       bd-42 (review)                                 │
│ 14:54:18 cleaner  bd.create   bd-59 chore "Containerfile drive-by"  blocks 42│
│ 14:54:22 cleaner  message     → max "see bd-59"                              │
│ 14:55:44 bob      bd.create   bd-58 bug  "synth mode hardcoded" blocks 42    │
│ 14:55:50 bob      message     → max "see bd-58"                              │
│ 14:56:12 reviewer bd.create   bd-60 bug  "no test for ws-id branch"          │
│ 14:56:14 reviewer message     → max "see bd-60"                              │
│ 14:58:02 max      claim       bd-58                                          │
│ ...                                                                          │
│                                                                              │
└─[live ↓ tail]────────────────────────────────────────────────────────────────┘
 t:back to summary  /:search  f:filter  p:pause/resume  e:export  c:copy line
```

### Tab 3 — Commits (real-time, agent-attributed)

```
┌─[F1]Dashboard─[F2]Events─[F3]Commits─[F4]PRs─[F5]Agents──────────────────────┐
│ Commits (real-time · main+all agent branches)     filter:[all] · group:agent │
│ ──────────────────────────────────────────────────────────────────────────── │
│  ago    agent     sha     branch         subject                       bd    │
│ ──────────────────────────────────────────────────────────────────────────── │
│  3m12s  max       a3f8e9  max/bd-42      bd-42: resolve agent ref by..  42   │
│  3m04s  max       b1c220  max/bd-42      bd-42: regression test for..   42   │
│  9m41s  bob       7e6311  bob/bd-31      bd-31: tighten architect prompt 31  │
│ 11m08s  max       0d2ee4  max/bd-31      bd-31: add code-agent.md       31   │
│ 14m22s  max       4c8a01  main           bd-29: cli prints role path    29   │
│ 23m17s  cleaner   —       —              (no commits this hour)         —    │
│ 31m01s  reviewer  —       —              (no commits this hour)         —    │
│                                                                              │
│ Showing 6 of 18 commits in last hour.  [↑/↓] navigate  [Enter] open in gh    │
└──────────────────────────────────────────────────────────────────────────────┘
 g:group(agent|branch|hour)  /:search  d:diff  o:open in gh  c:copy sha
```

### Tab 4 — PRs

```
┌─[F1]Dashboard─[F2]Events─[F3]Commits─[F4]PRs─[F5]Agents──────────────────────┐
│ Pull Requests           filter:[ready]  [all] [mine] [stuck]    poll:60s ↻   │
│ ──────────────────────────────────────────────────────────────────────────── │
│  #    bd     branch          author     review  ci   merge   age    state    │
│ ──────────────────────────────────────────────────────────────────────────── │
│ #91   42     max/bd-42       max         ✓      ✓    ✓       4m   →READY    │
│ #88   31     bob/bd-31       bob         ✓      ✓    ✓      27m   →READY    │
│ #93   58     max/bd-58       max         ·      ⟳    ✓       1m    awaiting │
│ #87   29     max/bd-29       max         ✗      ✓    ✓     2h05m    changes │
│ #82   25     bob/bd-25       bob         ✓      ✗    ✓       3d05    ci-red │
│                                                                              │
│ ──────────────────────────────────────────────────────────────────────────── │
│ Selected #91 — "bd-42: resolve agent ref by name or ws id"                   │
│  Approvals: reviewer ✓  bob ✓                                                │
│  Checks:    cargo-test ✓  cargo-clippy ✓  build ✓                            │
│  Mergeable: yes           Merge-slot: held by max                            │
│  URL:       https://github.com/msumme/agent-in-docker/pull/91                │
└──────────────────────────────────────────────────────────────────────────────┘
 Enter:open  A:approve  M:merge  c:copy url  r:request changes  /:search
```

### Tab 5 — Agents (detail + kill/restart)

```
┌─[F1]Dashboard─[F2]Events─[F3]Commits─[F4]PRs─[F5]Agents──────────────────────┐
│ Agents (4 connected · auto-restart at 90% context)         threshold:[90%]   │
│ ──────────────────────────────────────────────────────────────────────────── │
│ ┌────────────────────────────────────────────────────────────────────────┐   │
│ │ ●max              producer       PID 4711   container max               │   │
│ │   Branch    max/bd-42                              Mode    long-running │   │
│ │   Last cmd commit a3f8e9               Last seen   2m12s                │   │
│ │   Context  ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  12%   24k / 200k tok     │   │
│ │   Bd-claim bd-42 (in_progress)         Holder of merge-slot             │   │
│ │   [a]ttach  [k]ill  [R]estart  [c]lear-context  [m]essage               │   │
│ └────────────────────────────────────────────────────────────────────────┘   │
│ ┌────────────────────────────────────────────────────────────────────────┐   │
│ │ ●reviewer         review-agent   PID 4793   container reviewer          │   │
│ │   Branch    —                                      Mode    long-running │   │
│ │   Last cmd read mcp.rs                 Last seen   12s                  │   │
│ │   Context  ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░  84%  168k / 200k tok ⚠near-cap │   │
│ │   Bd-claim bd-42 (review)                                               │   │
│ │   [a]ttach  [k]ill  [R]estart  [c]lear-context  [m]essage  [!]auto-rst  │   │
│ └────────────────────────────────────────────────────────────────────────┘   │
│ (▼ bob, cleaner collapsed — Tab to expand)                                   │
└──────────────────────────────────────────────────────────────────────────────┘
 N:new  k:kill  R:restart  c:clear  T:set threshold  Enter:expand  Tab:cycle
```

## Work breakdown

Six independent epics. A–D deliver the TUI control center. E is the bd ACL
gateway that makes coordination trustworthy under untrusted agents (which,
in practice, includes all of them — agents make mistakes whether or not
they are aligned). F is the Teams model that turns the workflow from
"individual agents in a project" into "ephemeral teams that own one PR
each from ticket to merge." A–D can parallelize; E and F are independent
and can land in any order, but F depends on E for the bd ACL surface and
on D for the lifecycle/compaction primitives.

### Pipeline (the shape A–F serve)

```
                     ┌── kick-back to planner if shape needs re-thinking ──┐
                     ▼                                                      │
┌────────┐   ┌────────┐   ┌──────────┐   ┌──────────┐   ┌──────────┐   ┌────────┐
│ TICKET │──▶│PLANNER │──▶│ PRODUCER │⇄ │ REVIEWER │──▶│  HUMAN   │──▶│ MERGED │
│        │   │ (spec) │   │  (code)  │  │(verify)  │   │ (merge)  │   │        │
└────────┘   └────────┘   └──────────┘   └──────────┘   └──────────┘   └────────┘
                                ▲              │
                                └──────────────┘
                              loop until approved
```

Three roles, one human gate. Planner files a spec ticket; producer
implements against the spec; reviewer verifies; human merges. Reviewer
folded the previous architect+cleaner concerns. Architect's structural
review moved earlier (planner) where rejection is cheapest. Cleaner's
simplification became one of the reviewer's checks.

The unit of work is one PR; one PR maps to exactly one bd ticket. Epics
produce stacks of PRs, one per child ticket.

### Epic A — Event store & commit log

The foundation. Everything downstream consumes structured events.

- Define an `OrchestratorEvent` schema covering bd state changes, agent
  status, message traffic, branch/commit activity, PR opens, merge-slot
  transitions, system events.
- Persist events to a ring-buffer in memory (last N=10k) plus an append-only
  jsonl on disk for replay.
- A commit watcher polls `git log --all --format='%H %an %ct %s' --since=...`
  every 5s; emits one event per new commit.
- Per-agent `git config user.name` set during container startup so author
  attribution is reliable even on `main`.
- Tab 2 (Events) and Tab 3 (Commits) are views over this store.

### Epic B — Summary narrator

- A summarizer task fires every 5 min OR after 25 events, whichever first.
- Reads last hour of events from the event store; calls Sonnet (cheap, with
  prompt caching on the role/preamble).
- Output written to `summary.json` plus pushed via the existing event
  channel as a `summary.updated` event.
- Dashboard tab reads the latest summary; `t` toggles to a raw events tail
  in the same panel.
- Threshold for cost cap: max 1 call/min. Skip if no new events.

### Epic C — gh poller & PR panel

- `gh pr list --state open --json number,title,headRefName,author,reviewDecision,statusCheckRollup,mergeable,url`
  every 60s. Map `headRefName` to the agent owning that branch
  (convention: `<agent>/<bd-id>`).
- Filter "ready" = `reviewDecision == APPROVED && all checks green &&
  mergeable == MERGEABLE && no agent-only label`.
- Render in Dashboard sidebar (just titles, max 4) and in Tab 4 (full table).
- Hotkeys: `Enter` runs `gh pr view --web`, `A` approves, `M` merges
  (confirms first), `c` copies URL.
- Terminal hyperlinks via OSC 8 so URLs are clickable in iTerm2/modern
  terminals.

### Epic D — Agent lifecycle: context, restart, bd identity, claim release

This epic owns everything that happens around an agent's lifetime:
context budget, restart/clear semantics, identity in bd, and what happens
to the agent's outstanding work on disconnect.

#### D.1 — Context tracker and budget bars

- Per-agent context tracker reads
  `<agent_dir>/projects/<workspace>/conversation.jsonl`, tallies token usage
  with a heuristic (4 chars/token initial; switch to tiktoken if accuracy
  needed). Refresh every 10s.
- Tab 5 (Agents) renders progress bar + raw counts.
- Threshold (default 60%) is configurable per role. At threshold the
  orchestrator dispatches `/compact` to the agent. If usage doesn't drop
  below 35% within 30s, it escalates to hard restart. Natural-break
  events (PR open, ticket close, suspend) also trigger `/compact`
  regardless of threshold.

#### D.2 — Restart and clear-context controls

- `c` (clear-context) sends `/clear` via tmux to the agent — preserves
  process, container, and bd claim; drops conversation. Logged as
  `agent.cleared` event.
- `R` (restart) hard-restarts the container; re-runs the same `start_agent`
  payload; preserves persisted role.
- `k` (kill) just stops; user explicitly relaunches.
- Auto-compact: at the 60% threshold OR at a natural break, the
  orchestrator sends `/compact`. If `/compact` doesn't drop usage below
  35% within 30s, it falls back to hard restart. Natural breaks come
  from event-store events: `pr.opened`, `bd.ticket_closed`,
  `team.suspending`.

#### D.3 — bd identity per agent (now-scope)

bd identifies whoever runs the command via `--actor`, defaulting to
`$BD_ACTOR` → `git user.name` → `$USER`. Set both explicitly per agent so
every bd action and every git commit attribute correctly:

- In `core/src/types.rs::container_run_args`, add
  `-e BD_ACTOR=<agent_name>` to the container env.
- In `entrypoint/src/main.rs` (or `setup.rs`), run
  `git config --global user.name <agent_name>` and
  `git config --global user.email <agent_name>@agents.local` before the
  WS register.
- Document in `_meta.md`: agents must never pass `--actor` themselves;
  bd will default correctly. (Enforcement comes in Epic E; this is the
  convention layer.)

Result: `bd update bd-42 --claim` from inside max's container records
`assignee=max`; `bd update bd-42 --claim` from bob's container *fails*
because the claim is atomic. Commit attribution on Tab 3 (Commits) works
because `git log` author matches the agent name.

#### D.4 — Claim release on disconnect (now-scope)

bd has no auto-expiry on claims. If an agent crashes mid-pass, its
`assignee=<agent>, status=in_progress` ticket is frozen until a human
intervenes. The orchestrator already knows when an agent disconnects —
`AgentManager::agent_disconnected` (`agent_manager.rs:208`). Hook release
in there:

- On disconnect, query `bd query "assignee=<agent> AND status=in_progress"`.
- For each result, run `bd update <id> --assignee=none --status=open
  --reason="reaper: <agent> disconnected"`.
- Emit `claim.released` events to the event store so Tab 2 shows the
  recovery.
- Startup reaper: at orchestrator boot, scan all `status=in_progress`
  tickets; release those whose assignee is not currently connected.
  Handles the case where the orchestrator itself crashed.

#### D.5 — Role prompts: claim discipline

Update `_meta.md` and the producer prompts:

- Use `bd update <id> --claim` instead of `bd assign + set-state`. The
  atomic single-call CAS is what prevents two agents from racing.
- Use `bd close <id> --claim-next` to chain finish+claim atomically; no
  idle window between tickets.
- Document the release-on-disconnect rule so agents understand they don't
  need to manually unclaim on shutdown.

#### Acceptance for Epic D

- Two agents trying `bd update bd-X --claim` simultaneously: exactly one
  succeeds.
- Killing an agent mid-claim: within 5s of disconnect, the ticket returns
  to `assignee=none, status=open`.
- An agent at 92% context auto-clears; if still above 50% after 30s,
  container restarts, ticket claim re-applied (or released and reclaimed,
  depending on how restart routes through agent_disconnected).
- All commits on agent branches show the agent name as author in
  `git log` and on Tab 3.

### Epic E — bd MCP gateway (ACL on coordination)

Epic D gives every agent correct identity and reaps stuck claims, but
nothing prevents an agent from issuing `bd close --force` to bypass an
unsatisfied review gate, calling `bd update --actor=somebody-else` to
spoof identity, or running `bd delete` on someone else's ticket. Even
well-aligned agents make mistakes; treating all agents as untrusted is
the right default.

This epic moves bd from a CLI inside the container to an MCP tool surface
on the host, with per-role allowlists. Containers lose direct bd access.

#### E.1 — Remove bd from the container image

- Drop `bd` and `dolt` binaries from `Containerfile.base`. Containers can
  no longer invoke `bd` directly.
- Remove the dolt-port env wiring from container args (only the host
  needs to talk to dolt now).

#### E.2 — MCP tool surface

Add to the orchestrator's MCP server:

| MCP tool | Maps to | Notes |
|---|---|---|
| `bd_query(filter, fields?)` | `bd query` | All roles |
| `bd_show(id)` | `bd show` | All roles |
| `bd_ready()` | `bd ready` | All roles |
| `bd_create(type, title, description, deps?, parent?, ext_ref?)` | `bd create` | Type whitelisted per role |
| `bd_claim(id)` | `bd update --claim` | Producers only |
| `bd_close(id, reason)` | `bd close` | Producers only; `--force` never |
| `bd_close_claim_next(id, reason)` | `bd close --claim-next` | Producers only |
| `bd_set_state(id, dimension, value, reason)` | `bd set-state` | Dimension whitelisted |
| `bd_update_field(id, field, value)` | `bd update` | Field whitelist; never `--actor` |
| `bd_gate_add(ticket_id, reason)` | `bd gate ...` | Producers only |
| `bd_gate_resolve(gate_id)` | `bd gate resolve` | Reviewers only; must be linked to a review ticket they own |
| `bd_note(id, text)` | `bd note` | All roles |
| `bd_comment(id, text)` | `bd comment` | All roles |

Every tool invocation:

1. Reads the calling agent's name from the `x-agent-name` MCP header (the
   same header `message_agent` already uses).
2. Looks up that agent's role.
3. Checks the requested verb is in the role's allowlist.
4. Strips forbidden flags (`--force`, `--actor`, `--db`, `--no-history`,
   `--readonly`-as-bypass) before exec.
5. Forces `BD_ACTOR=<agent-name>` on the host invocation, ignoring any
   value the agent tried to pass.
6. Emits a `bd.<verb>` event to the event store (Epic A) for full
   audit visibility.

#### E.3 — Per-role allowlist

Living next to the existing `permissions/` config (`file_read_paths`,
`git_push_remotes`). Add a `bd_actions` block to each role's `.yml`:

```yaml
# roles/feature-producer.yml
bd_actions:
  query: true
  show: true
  ready: true
  create:
    types: [feature, task, decision]
  claim: true
  close: { allow_force: false }
  set_state:
    dimensions: [review]
  update_field:
    fields: [description, design, notes, acceptance, due, priority]
  gate_add: true
  gate_resolve: false
  note: true
  comment: true
```

Reviewers get a different shape (no `claim`/`close` of producer tickets;
yes to `gate_resolve` but only on review tickets they own).

#### E.4 — Gate-resolve linkage rule

To prevent a reviewer from resolving an arbitrary gate, `bd_gate_resolve`
walks the dependency graph: the gate must be on a ticket where this
reviewer is the assignee of a related review-task ticket
(`type=task AND parent=<ticket-id> AND assignee=<reviewer>`), or the
ticket itself was filed by this reviewer. Otherwise reject.

#### E.5 — Human escape hatch in TUI

The TUI keeps unrestricted bd access. New hotkeys on Tab 4 (PRs) and
Tab 5 (Agents):

- `F` — force-close the selected ticket (with confirmation).
- `S` — steal a stuck claim (with confirmation; reason recorded).
- `G` — manually resolve a gate (with confirmation).

Operators can always cut through; agents never can.

#### E.6 — Audit replay

Because every MCP bd call emits an event, the event store (Epic A) becomes
the audit log. Tab 2 (Events) gains a filter `role:bd` that surfaces
exactly what each agent did via the gateway. Anomalies (e.g. an attempted
forbidden flag) emit a `bd.denied` event for forensics.

#### Acceptance for Epic E

- Container has no `bd` binary and no `dolt` binary; agents cannot invoke
  bd directly.
- A producer attempting `bd_close` with `--force` in any field gets
  `denied: forbidden flag` and a `bd.denied` event.
- A reviewer attempting `bd_gate_resolve` on a gate they have no review
  link to gets `denied: not authorized for gate <id>`.
- Every successful and denied bd action shows up in Tab 2 with the
  agent name and verb.
- The TUI human can still `bd close --force` via the `F` hotkey (with
  confirmation).

### Epic F — Teams (PR-scoped agent groups with worktrees and suspend/resume)

A **team** is a group of three agents (planner, producer, reviewer)
spawned to take one bd ticket from spec to merge. The team's identity is
the ticket id (`team-bd-42`). When the PR merges, the team is destroyed.
When the PR is open and waiting on humans, the team self-suspends and
preserves state so it can wake when humans act — even days later.

This epic delivers the lifecycle, isolation (worktrees), persistence
(suspend/resume), and TUI integration to make multiple PR-scoped teams
work in parallel on a single project without colliding.

#### F.1 — Team lifecycle

States: `spawning → active → suspended ⇄ active → completed | failed`.

```
                        ┌──────── spawn(bd-42) ────┐
                        ▼                          │
                ┌──────────────┐                   │
                │  spawning    │                   │
                └──────┬───────┘                   │
                       │ all 3 agents connected    │
                       ▼                           │
                ┌──────────────┐                   │
              ┌▶│   active     │◀─resume(team-id)──┤
              │ └──────┬───────┘                   │
              │        │                           │
              │  ┌─────┴────┐                      │
              │  │          │                      │
              │  ▼          ▼                      │
              │ pr.opened   blocked/idle           │
              │ │           (24h waiting on        │
              │ │            human review,         │
              │ │            or self-suspend)      │
              │ │           │                      │
              │ │           ▼                      │
              │ │   ┌──────────────┐               │
              │ │   │  suspending  │               │
              │ │   └──────┬───────┘               │
              │ │          │ snapshot+kill         │
              │ │          ▼                       │
              │ │   ┌──────────────┐               │
              │ │   │  suspended   │───────────────┘
              │ │   └──────┬───────┘
              │ │          │ pr.event (review/merge/close)
              │ │          ▼ wake watcher
              │ └────► back to active (or completed)
              │
              │ pr.merged
              ▼
       ┌──────────────┐
       │  completed   │ ──► archive + teardown
       └──────────────┘
```

#### F.2 — `TeamManager`

New module in `core/`, peer to `AgentManager`. Owns:

- `.teams/<team-id>/manifest.json` per team — ticket id, PR url, state,
  base branch, work branch, role list, timestamps, suspend reason.
- Spawn/suspend/wake/teardown commands.
- Wake watcher: subscribes to PR-status events from Epic C; matches
  events to suspended teams; triggers wake.
- Boot scan: on orchestrator startup, reads `.teams/`, registers teams
  in their last-known state, restarts any that were `active` at crash
  (idempotent — they re-claim their spec/branch/PR).

Emits team events to Epic A's event store: `team.spawned`,
`team.active`, `team.suspending`, `team.suspended`, `team.waking`,
`team.completed`, `team.failed`.

#### F.3 — Worktrees (parallel teams without filesystem collision)

Currently every agent mounts the same project directory at `/workspace`.
Two producers running `git checkout` on different branches would race.
Worktrees solve this: each team gets its own working copy of the repo
on its own branch, all sharing the same `.git/objects` store.

Convention:

```
<project-root>/
  .git/                     # shared object store
  .beads/                   # shared bd database
  .teams/                   # team manifests + per-role state
  .teams-worktrees/         # one worktree per team
    team-bd-42/             # checked out to branch team-bd-42/code
    team-bd-58/             # checked out to branch team-bd-58/code
```

On `team.spawn(bd-42)`:

1. `git worktree add .teams-worktrees/team-bd-42 -b team-bd-42/code <base>`
2. Each agent in the team mounts that worktree path as `/workspace`.
3. Producer commits there; worktree's HEAD moves; main project tree
   is untouched.

On `team.completed` or `team.failed`:

1. `git worktree remove .teams-worktrees/team-bd-42` (or `--force` if
   the team failed mid-pass and the worktree has uncommitted state —
   archived to `.teams/<team-id>/archive/` first).
2. Optionally delete the team branch if PR merged (use squash-merge
   convention so the working branch can be safely pruned).

The bd database stays at `<project-root>/.beads/`; teams access it via
the same `DOLT_HOST` / `DOLT_PORT` env vars the agents already get.

Constraint: a branch can only be checked out by one worktree at a time.
The `team-<id>/code` naming is unique by construction. If a stale
worktree exists (orchestrator crashed), the boot scan reclaims it.

#### F.4 — Suspend/resume mechanics

Self-suspend: any agent in a team can call the `team_suspend(reason)`
MCP tool. The orchestrator:

1. Sends `/compact` to all three agents in the team (preserve summary).
2. Snapshots each agent's `conversation.jsonl` to
   `.teams/<team-id>/<role>/conversation.jsonl`.
3. Writes `manifest.json` with state=`suspended`, the reason, the wake
   conditions to watch, and the last-active timestamp.
4. Calls `bd set-state <team-ticket> team=suspended`.
5. Stops all three containers (`podman rm -f`).
6. Closes the team's tmux windows.

The worktree stays. The team branch stays. The PR stays. Only the
agents go.

Resume happens when the watcher matches a wake condition:

- `pr.review.changes_requested` → wake the producer only.
- `pr.review.approved` → wake the reviewer briefly to push "human
  approved, awaiting merge."
- `pr.merged` → transition straight to `completed`, no wake.
- `pr.closed` (without merge) → transition to `failed`.
- `bd.note_added` on the team's spec ticket with `redesign-needed` →
  wake the planner.

Wake protocol:

1. Restart the relevant container(s) with the team's state dir mounted.
2. Inject a resume primer as the initial prompt, e.g.
   ```
   You are resuming work on bd-42 / PR #91. Role: producer.
   Last status: PR opened, awaited reviewer.
   Wake reason: human requested changes — see PR #91 comments.
   Your prior conversation summary is loaded.
   Continue your role.
   ```
3. The compacted prior conversation gives the agent its synthesis;
   the primer gives it the trigger event.
4. Roles whose work isn't needed yet stay suspended.

Partial wake — only one or two of the three roles active at a time —
is the common case. Three-way active is brief and intentional.

#### F.5 — `team_*` MCP tools

| MCP tool | Caller | Effect |
|---|---|---|
| `team_spawn(ticket_id, base_branch?)` | TUI / dispatcher | Creates worktree, spawns 3 agents, claims ticket |
| `team_suspend(reason)` | any team agent | Compact, snapshot, kill containers |
| `team_resume(team_id, role?)` | TUI / wake watcher | Restart the named role with primer; default all |
| `team_complete(team_id)` | wake watcher (on pr.merged) | Archive, remove worktree, teardown |
| `team_kill(team_id)` | TUI human only | Force teardown without archive (failed runs) |

These flow through Epic E's bd MCP gateway: `team_spawn` calls
`bd_claim` server-side; `team_suspend` calls `bd_set_state`;
`team_complete` calls `bd_close`. Agents don't see raw bd from
inside the team flow — only the team verb.

#### F.6 — TUI integration

Tab 5 (Agents) groups by team:

```
Team team-bd-42  status:active  PR #91  branch:team-bd-42/code
  ├ planner    suspended  spec: bd-43
  ├ producer   active     ░░░ 41% context
  └ reviewer   suspended

Team team-bd-58  status:suspended  PR #93  awaiting human
  └ (all suspended — wake on PR event)
```

New hotkeys:

- `T` (on Tab 4 / PRs or Tab 5 / Agents) — `team_spawn` for selected
  ticket. Pops a confirm with base-branch override.
- `s` (on Tab 5) — `team_suspend` selected team.
- `w` (on Tab 5) — `team_resume` selected suspended team.
- `K` (on Tab 5) — `team_kill` (with confirm; failed-run teardown).

Tab 4 (PRs) status icons grow a team-state column: `●` active team,
`◑` suspended, `✓` completed, `·` no team (PR opened directly by human).

#### F.7 — Stacked PRs (cross-cutting with F.3)

For epic children, teams stack worktrees:

```
main
  └── epic-bd-100/base                (worktree shared by all child teams)
        ├── team-bd-101/code  (PR #200 against epic-bd-100/base)
        ├── team-bd-102/code  (PR #201 against team-bd-101/code)
        └── team-bd-103/code  (PR #202 against team-bd-102/code)
```

When PR #200 merges into `epic-bd-100/base`, an orchestrator hook runs
`git rebase --update-refs` on team-bd-102's branch and force-pushes —
the producer in team-bd-102 sees the rebase reflected on next wake. The
final PR is `epic-bd-100/base → main` and goes through one more review
loop (a fresh team for the epic itself, or a manual human merge).

Stacking is opt-in per team. The default `team_spawn(ticket)` uses
`main` as base; `team_spawn(ticket, base=epic-bd-100/base)` opts in.

#### Acceptance for Epic F

- Two teams (`team-bd-42`, `team-bd-58`) spawned in parallel on the
  same project. Their producers commit concurrently to different
  branches without collision; both PRs open green.
- Producer in `team-bd-42` calls `team_suspend` after PR open. Within
  10s: all three containers killed, manifest written, tmux windows
  closed. `team-bd-58` continues unaffected.
- A reviewer (human) requests changes on PR #91. Within 60s of the gh
  poller catching the event: `team-bd-42`'s producer wakes with the
  resume primer, addresses the request, pushes a new commit, suspends
  again.
- PR #91 merges. Within 60s: `team-bd-42` transitions to `completed`,
  worktree removed, manifest archived. The team disappears from Tab 5.
- Orchestrator killed and restarted: all teams that were `active` come
  back active; all teams that were `suspended` stay suspended; the
  wake watcher resumes from manifest.

## Hard parts and decisions

- **Context %.** Claude Code doesn't expose this to MCP today. Read the
  conversation.jsonl directly. Heuristic for now (4 chars ≈ 1 token);
  upgrade to tiktoken-rs if it matters. Acceptable error for a UX hint.
- **Auto-restart vs auto-clear.** Default to `/clear`; only restart if
  clear doesn't help. Restart loses the bd claim if not careful — confirm
  the claim survives container restart (depends on Epic A's event log
  having the claim event so we can reapply on relaunch).
- **gh auth.** Polling needs `gh auth status` working on host. Document
  that the orchestrator inherits host gh credentials; don't pass them to
  containers.
- **Event store on restart.** Replay from the jsonl on startup so the
  Dashboard isn't empty. Cap replay window to last 24h to avoid bloat.
- **PR ↔ ticket mapping.** Branch convention `<agent>/<bd-id>` is load-bearing.
  Producers' role prompts already enforce it; reject manual deviations.
- **TUI re-architecture.** Current TUI is single-screen. Tabs require a
  router pattern. Use ratatui's existing primitives; don't pull in a
  framework.
- **Trust model.** All agents are untrusted by default. Epic D establishes
  identity (`BD_ACTOR`) and lifecycle hooks; Epic E enforces what each
  identity is allowed to do via the bd MCP gateway. Until E lands, an
  agent that goes off-rails could `bd close --force` past gates or spoof
  `--actor`. Acceptable risk during initial rollout when humans are
  watching the TUI; not acceptable for unattended operation.
- **Claim release vs. restart.** When the orchestrator restarts an agent
  (Epic D auto-restart), the WS disconnect fires before the new container
  registers. The reaper releases the claim; the new container then has
  to re-claim. This is correct (handles real crashes uniformly) but
  means restart adds a small window of "claim is open, anyone can grab
  it." Acceptable; a reviewer briefly claiming a producer's ticket would
  be visibly weird and easy to recover from.
- **Worktrees and disk usage.** Each team's worktree is a full checkout
  of the working files (not the .git objects, which are shared). For a
  ~100MB repo and 5 concurrent teams, that's ~500MB of duplicate working
  trees. Acceptable at our scale; revisit if teams grow into the dozens.
  `git worktree prune` on orchestrator boot to reap stragglers.
- **Stale worktrees on crash.** If the orchestrator dies while a team
  is mid-pass, the worktree stays. Boot scan reads `.teams/` and
  reconciles: existing worktrees with manifests are reattached;
  worktrees without manifests are pruned; manifests without worktrees
  trigger team teardown. Guard against `git worktree add` failing on
  a path that already exists (it does, by default — handle the error).
- **Compaction interaction with team suspend.** Suspend always calls
  `/compact` first to ensure the snapshot is the summary, not the
  verbatim conversation. If `/compact` fails (rare), suspend falls
  back to snapshotting verbatim — better than losing state. Resume
  primer notes "summary unavailable, raw conversation loaded" so the
  agent knows.
- **Wake-storm risk.** A burst of PR events (e.g., a CI run finishing
  for many PRs at once) could wake many teams simultaneously. Cap
  concurrent active teams (default 5) and queue waking ones. The
  rest stay suspended until capacity opens.

## Open questions

- Do we want the Dashboard summary to be persistent between TUI restarts,
  or always regenerated on first launch? (Lean: persist; show stale
  indicator if older than threshold.)
- Should `kill` and `restart` require confirmation? (Lean: yes for kill,
  no for clear-context.)
- Do we surface ALL PRs or only those tied to a `bd-` branch? (Lean: all,
  but rank/highlight bd-tied ones.)
- Cost ceiling on summarizer per day? (Lean: configurable env var,
  default $5/day.)
- Should Epic E's bd MCP tools also expose `bd merge-slot acquire`?
  (Lean: yes for producers, with the same gate the existing role-prompt
  rule implies. Steal/release stays in the TUI human escape hatch.)
- For the gate-resolve linkage rule, is "filed by this reviewer" enough,
  or do we require an explicit review-task assignment? (Lean: require
  the review-task; "filed by" is too easy to game by filing one yourself
  to gain authority.)
- Should the dispatcher that spawns teams be a human in the TUI, or an
  agent ("router") that picks `bd ready` items and spawns teams
  automatically? (Lean: TUI hotkey first, autonomous router as a later
  feature once we trust the team flow end-to-end.)
- Should team agents share a `team-memory` directory analogous to the
  existing role-memory? (Lean: yes — facts the planner discovers should
  carry to the producer and reviewer without re-reading the spec.
  Lives at `.teams/<team-id>/shared-memory/`.)
- For stacked PRs across teams, who does the rebase when an upstream
  PR merges — orchestrator hook, or wake the producer to do it? (Lean:
  orchestrator hook for the rebase-and-push; no agent token spend on
  mechanical work.)
- What's the right cap on concurrent active teams? (Lean: configurable;
  default 5. Past that, queue. Memory-bound by container count and
  worktree disk.)

## Acceptance criteria

- All five tabs render without panics; switching tabs is sub-100ms.
- Commits authored by an agent show that agent's name in Tab 3.
- Auto-restart at threshold works: agent at 95% gets `/clear`'d; if still
  above 50% after 30s, container restarts and re-claims its bd ticket.
- A PR that is approved + green + mergeable shows up in the Dashboard
  "ready for you" list within 60s of becoming ready.
- The narrator produces a summary at startup (or shows "no recent
  activity") and refreshes every 5 min while events flow.

## Out of scope

- Web UI. The store-as-source-of-truth pattern leaves the door open, but
  not building it now.
- Cross-project orchestration (multiple bd databases). Single-project for
  now; per-project orchestrator is a separate epic.
- Editing PR descriptions or bd ticket bodies from the TUI. Read-only +
  approve/merge actions only.
- Slack/Discord notifications. The event stream supports them, but
  surfacing belongs in the TUI first.
