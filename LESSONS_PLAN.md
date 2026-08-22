# Lessons-as-proposals: replace shared role memory

Ticket: `agent-in-docker-3r1`. Background: `.analysis/2026-08-21-state-and-vision.md` §6.

## Why

Role memory (`.agents/_role-memory/<role>/`, mounted at `/root/.claude/projects`
for every agent of that role) lets agents write their own durable lessons into
an unversioned, machine-local, Claude-specific side-channel. This:

1. Hides learnings from the repo — the two lessons feature-producer actually
   recorded ("no AI attribution", "no platform-rollup workaround") are
   instructions that belong in `_meta.md`/role prompts, and the memory channel
   absorbed them instead.
2. Couples to Claude Code's `~/.claude/projects` layout; other runtimes get nothing.
3. Keys learnings to this machine and shares one pool per role name across projects.
4. Bypasses human review — the system grades its own homework, which defeats
   the trust ledger the project exists to build.

Replacement: learnings become **proposal files committed on the team's work
branch**, reviewed by the human at integration, and folded into version-
controlled instruction files. Session state (resume) is preserved per-agent.

## Design

### The proposal file

An agent that hits a lesson-worthy moment writes one file per lesson:

```
.agents/lessons/proposed/<slug>.md        (in its /workspace clone)
```

committed alongside its normal work, so it arrives in the exact diff the host
already reviews (`team integrate` / PR). Format:

```markdown
---
role: feature-producer
ticket: agent-in-docker-3r1
scope: project        # or: tool
target: roles/feature-producer.md
---
<What happened, in one or two sentences: the gap, how it surfaced.>

**Proposed instruction:** <the exact text or example to add to the target file.>
```

- `scope: project` — a convention of the codebase being worked on (idioms,
  build quirks, domain language). `target` is a file in that repo
  (typically `.agents/roles/<role>.md` override or a conventions doc).
- `scope: tool` — the agent system itself has a gap (role prompt unclear,
  missing example, wrong instruction, missing pipeline step). `target` names
  the agent-in-docker file (`roles/<role>.md`, `roles/_meta.md`).
- One lesson per file. The agent proposes; it never applies the change itself.

### Human routing at review

Scope is a *declaration*, not a write location — a sealed agent can only write
to the target project's clone. At integration the human:

- accepts → folds the proposed instruction into the target file (for
  `scope: tool`, by editing agent-in-docker's repo) and deletes the proposal;
- rejects → deletes the proposal (optionally with a `bd note` on why).

Folding happens via human commits, so `git log` over `.agents/lessons/` and
the instruction files **is the trust ledger**: every accepted lesson is a
recorded instance of the system needing correction.

No `accepted/` archive directory — git history is the archive. No scaffolding
seeded into projects — the convention text tells the agent to create the path
(`mkdir -p`) when it first files one.

### `_meta.md` addition (new section, after "Messaging")

```markdown
### Lessons

When you learn something future agents should know — a review blocker
revealed a rule you didn't know, you had to guess at a project convention,
an instruction you were given was wrong or incomplete — do not trust memory
to carry it. File a lesson proposal.

Write `.agents/lessons/proposed/<slug>.md` in your clone (create the
directory if needed) and commit it with your normal work:

    ---
    role: <your role>
    ticket: <bd id that surfaced this>
    scope: project | tool
    target: <file the instruction belongs in>
    ---
    <What happened, in one or two sentences.>

    **Proposed instruction:** <exact text or example to add to the target.>

- `scope: project` — a convention of this codebase. Target is a file in
  this repo.
- `scope: tool` — the agent system has a gap (role prompt unclear, missing
  example, wrong instruction). Target names the role file.
- One lesson per file; check existing proposals before filing a duplicate.
- Propose, never apply: the human reviews at integration and folds accepted
  lessons into the target. Your job ends at the committed proposal file.
```

### Rust changes: remove the shared mount, keep resume

Key simplification: `agent_dir` is already bind-mounted at `/root/.claude`
(`types.rs:154`). Deleting the role-memory mount means Claude Code writes
sessions to `agent_dir/projects/` on the plain `agent_dir` mount — **per-agent
session state and `--continue` resume survive with no replacement machinery**.

Removals:

- `project_config::setup_role_memory_dir` (`project_config.rs:164-172`) and
  its callers (`cli/src/main.rs:191`, `team_cmd.rs:389,503`).
- `StartAgentPayload.role_memory_dir` field (`types.rs:107`) and the
  `-v {role_memory_dir}:/root/.claude/projects:Z` mount (`types.rs:156`).
- Test fixtures referencing `role_memory_dir` (`types.rs:413,440,474,501,521`),
  `team_cmd.rs:219,252`.

Consequences to verify in the change:

- Per-agent memory still self-accumulates inside `agent_dir/projects/` — but
  scoped to that agent instance's lifetime (team agent dirs are archived with
  the team), so nothing durable bypasses review anymore. Acceptable.
- `StartAgentPayload` is built fresh per launch (not persisted in manifests),
  so removing the field is not a serde-compat break — confirm no stored JSON
  embeds it before deleting.

### Migration / cleanup

1. Fold the two existing feature-producer lessons into their proper homes
   (`_meta.md` already covers AI attribution via review standards — verify;
   the rollup workaround lesson belongs in the producer role or project doc).
2. Delete `.agents/_role-memory/` (including orphaned dirs for pruned roles:
   architect, cleaner, producer).
3. Update `ARCHITECTURE.md`/`README.md` mentions of role memory if any.

## Phases (each compiles and passes tests)

1. **Rust removal** — drop `role_memory_dir` end-to-end; fix tests. Pure
   structural change, no prompt edits.
2. **Prompt convention** — add the Lessons section to `_meta.md`.
3. **Migration** — fold existing lessons, delete `_role-memory/`, doc updates.

## Open questions (for review before implementation)

1. Should the *reviewer* role prompt also get an explicit nudge — e.g. "if a
   blocker you filed reflects a rule the producer couldn't have known, file
   the lesson proposal yourself"? (Recommend yes: reviewers see the gaps most
   clearly, and Opus-tier reviewers will write better proposals than cheap
   producers.)
2. Verdict on per-agent self-memory: fine to leave team-lifetime-scoped as
   designed here, or should the entrypoint actively delete `memory/` dirs so
   agents can't self-remember at all? (Recommend leave it: ephemeral, and
   fighting Claude Code's built-in behavior is churn for no leakage.)
3. Should `bd` get a lightweight cross-link — e.g. the agent adds a
   `bd note <ticket> "lesson filed: <slug>"` — so lesson activity is visible
   in the tracker without duplicating content? (Recommend yes: one line.)
