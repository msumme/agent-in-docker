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

Four independent epics. Each is a few-day chunk; can parallelize across two
feature-producers.

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

### Epic D — Context tracking & restart controls

- Per-agent context tracker reads
  `<agent_dir>/projects/<workspace>/conversation.jsonl`, tallies token usage
  with a heuristic (4 chars/token initial; switch to tiktoken if accuracy
  needed). Refresh every 10s.
- Tab 5 (Agents) renders progress bar + raw counts.
- `c` (clear-context) sends `/clear` via tmux to the agent — preserves
  process and bd claim, drops conversation. Logged as `agent.cleared` event.
- `R` (restart) hard-restarts the container; re-runs the same `start_agent`
  payload; preserves persisted role.
- `k` (kill) just stops; user explicitly relaunches.
- Auto-restart: configurable threshold per role (default 90%). At threshold
  the orchestrator first sends `/clear`; if usage doesn't drop below 50%
  within 30s, hard-restart.

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
