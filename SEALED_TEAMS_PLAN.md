# Sealed Teams Plan

Living plan. Update the **Progress log** at the bottom as each step lands.

## North star

A supervised fleet of **sealed, role-pure agents** that build software with full
autonomy inside a credential-free sandbox, coordinate through **beads (bd)** as
the only bus, and reach the real world only through a narrow host-mediated gate.
The container is the security boundary; the orchestrator is the bridge between
the sandbox and git/GitHub. We stop putting a human in the loop on every host
action and instead seal the sandbox so there is nothing dangerous to gate except
the final integration.

This supersedes the per-action permission/approval machinery (MCP `git_push` /
`read_host_file` bridge, `NeedsApproval` → TUI oneshot) and the forge/ACL ideas
(too much setup for a tool that must work out-of-the-box on an existing repo).

## Target architecture

1. **Sealed sandbox.** Agents run in containers with no real-world credentials
   and no path to the real remote. The container boundary does the isolating.
2. **Per-agent isolated clone.** Each role gets its own `git clone --local` of
   the project (cheap, hardlinked objects), mounted alone. The agent commits to
   its own branch locally; it never pushes to a shared remote. This also fixes
   `agent-in-docker-crf` (worktree git pointer breaks inside the container).
3. **Host moves branches with fixed refspecs.** The trusted orchestrator fetches
   an agent's branch into the canonical repo and (on approval) merges to `main`.
   The branch namespace lives in the orchestrator's refspec, not anything the
   agent supplies — that is the per-branch guardrail, with zero server/hooks.
4. **bd is the bidirectional bus.** Agents signal and coordinate only through bd.
   The orchestrator mirrors review feedback *into* bd and reacts to bd state
   *out* to git actions. No agent→host RPC for normal work.
5. **Notification = park + reconcile.** An agent says "I'm done" by parking
   (sentinel file the host watches, or a bd state); it does not exit. The
   orchestrator reconciles the next action from **bd state (the truth)**, not
   from an exit code or a live channel. This removes the oneshot-timeout class of
   bug from the review.
6. **Role-split lifecycle.** Producer is **persistent** (suspend/resume with
   Claude session resume, so PR/review feedback lands in the *same context*).
   Reviewer is **ephemeral** (fresh container each round — unbiased eyes).
7. **Integration gate.** Normally: open a PR; the repo's existing branch
   protection is the real gate. **For dogfooding here:** skip PRs — the host
   merges the team's branch into `main` after a **review subagent** reviews the
   diff and feeds findings back into the team. Loop until clean.

## Dogfooding loop (how we build this)

We build the plan *using the teams*, integrating each piece through the same
loop we're building:

```
team works ticket in its clone/branch
      │  (commits locally, parks when done)
      ▼
host: `team integrate <id>`  → show diff vs main
      ▼
review subagent (Claude Code Agent tool) reviews the diff
      │
      ├─ findings → filed as bd beads on the ticket → team addresses → repeat
      └─ clean    → host merges branch → main (no PR)
```

Rule: **no human-in-the-loop per action**; the review subagent + final merge are
the only gates. When the team flow can't yet do a step, the main session does it
by hand *through the same loop* so we still exercise it. Prefer dispatching real
teams; fall back to hand-driving only to keep momentum.

## Phases (each independently works + is testable)

- [x] **P1 — Integration harness.** Host-side `team integrate <id>`: diff team
  branch vs `main`, and merge-to-`main` (`--no-ff`) on approval. Define the
  review-subagent procedure. *Test:* integrate a trivial change end-to-end.
  *This is the substrate that lets us dogfood every later phase.*
  → DONE: `core/src/integration.rs` (`MergeOps` trait + pure `integrate`, 5 unit
  tests) + `agent team integrate <id> [--merge]`. E2E-on-a-real-team deferred to
  P2 (need a team that produces a branch first).
- [ ] **P2 — Clone-per-agent isolation.** Replace the single shared worktree with
  `git clone --local` per role; mount each clone alone; host fetch-on-handoff
  helper. Fixes `crf`. *Test:* spawn a team, confirm each agent has an isolated
  repo with only its branch and can commit; host fetches the branch into
  canonical.
- [ ] **P3 — Park signal + bd reconcile.** Sentinel-file (inotify) or bd-state
  "I'm done"; orchestrator reconcile loop derives the next action from bd.
  Replace exit-as-signal. *Test:* agent parks, orchestrator advances the ticket
  without the agent exiting.
- [ ] **P4 — Persistent producer + ephemeral reviewer.** Producer suspends/
  resumes via Claude session resume; review feedback injected into the same
  session. Reviewer spawns fresh each round. *Test:* feedback round-trips into a
  resumed producer context; reviewer is a new container each time.
- [ ] **P5 — Cut the cruft.** Remove the now-unused `git_push`/`read_host_file`
  MCP bridge, `NeedsApproval` TUI path, `message_agent`, and the WS registry.
  *Test:* suite still green; teams still run; LOC drops materially.
- [ ] **P6 — Formula-driven dispatcher (Epic G).** Team shape becomes a bd
  formula; the orchestrator is a thin dispatcher (spawn container-steps, execute
  host-steps, await human/gate-steps). *Test:* `bd mol pour team` drives a full
  ticket through the loop.

## Risks / open questions

- Claude session resume fidelity for "same context" (P4) — verify `--resume`
  reloads enough transcript to be useful; if not, fall back to a compacted
  re-prime.
- `git clone --local` hardlink safety under agent `gc` (P2) — believed safe
  (hardlinks keep parent objects alive); verify.
- Whether the current team spawn even succeeds today given `crf` — P2 may need to
  come before any real team dogfooding works; discover early.

## Progress log

- 2026-06-03 — Plan written. Reviewed current `team_manager.rs` (single shared
  worktree per team; manifest at `.teams/<id>/manifest.json`; `conversation
  .jsonl` snapshot path already exists for resume). bd live on port 3307.
  Next: create bd epic + phase tickets, then implement P1.
- 2026-06-03 — bd epic `agent-in-docker-6mq` + phase tickets `6mq.1`..`6mq.6`
  created (dependency chain P1→…→P6). P1 implemented & tested: `integration.rs`
  (MergeOps trait, pure `integrate`, 5 green unit tests), wired as `agent team
  integrate`. Smoke-tested CLI (error path + `--help`). Core suite green.
  Next: P2 clone-per-agent — replace the shared worktree + `crf` mirror-mount
  hack with `git clone --local` per role; this is also the first real
  dogfooding candidate (spawn a team, integrate via the P1 loop).
</content>
