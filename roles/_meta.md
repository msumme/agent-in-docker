## Coding standards

These apply to every agent — producers writing code, reviewers judging it,
maintainers cleaning it. Internalize them; don't restate them.

### Domain-driven design

- Group code by business domain, not technical layer. A change to a feature
  should touch a small set of files in one place — if it scatters across
  many unrelated files, the boundaries are wrong.
- Use the language of the domain in names. Translate from concept to code,
  not from layer to layer.
- Make illegal states unrepresentable: prefer enums, sealed types, and
  algebraic data types over flag combinations and runtime checks.
- No dependency cycles between packages/modules/crates — ever.

### SOLID

- **S** — Each unit (function, class, module) has one reason to change.
  Orchestration (composes children, decides flow) and mechanics (does the
  thing) live in different functions, never mixed.
- **O** — Extend by adding new types/implementations, not by adding flags
  to existing ones.
- **L** — Subtypes/implementations of an interface must be drop-in
  substitutable. If a fake breaks behavior, the interface is wrong.
- **I** — Small, focused interfaces. Don't force callers to depend on
  methods they don't use.
- **D** — Depend on interfaces (traits/abstract types) for behavior, never
  on concrete implementations across module boundaries.

### Dependency injection and effects

- All dependencies are constructor-injected (or passed as parameters in FP
  styles). No `new` calls or globals reaching into infrastructure from
  business logic.
- Push side effects to the edges. I/O, network, clocks, randomness, and
  filesystem belong at the entrypoint of the application where wiring
  happens, not buried in business logic.
- If a unit cannot be tested without real infrastructure (database, real
  HTTP, real filesystem, real clock), its dependencies are wrong.

### Tests and determinism

- Test-driven: write a failing test before the production code, then make
  it pass. New code without tests is rejected on review.
- Deterministic by construction. Anything non-deterministic (clock, random,
  network, time-of-day) is injected so tests can control it. A flaky test
  is a design defect, not a "rerun it" event.
- Test behavior, not implementation. Assert on observable outcomes (what
  the user sees, what state results from an action), never on which private
  method was called.
- No mocks of code you own — use real implementations. Fake only at
  external boundaries (network, filesystem, clock), via injected interfaces.

### Naming and clarity

- Names over comments. If code needs a comment to explain *what* it does,
  rename or extract instead. Comments are for *why* (business rules,
  external constraints, non-obvious invariants).
- No anonymous expressions in parameter lists — extract to named locals.
- One level of abstraction per function. High-level orchestrators delegate;
  they do not also implement the mechanics.

### Things not to do

- Don't add error handling for conditions that can't happen.
- Don't add backwards-compatibility shims for code with one user.
- Don't extract abstractions until there are three concrete uses.
- Don't broaden scope mid-pass. Drive-by refactors get a ticket of their
  own; they do not ride along with feature work.
- Never add AI attribution to commits — no `Co-Authored-By`, no
  "Generated with Claude Code" line, nothing naming Claude or any AI.
  Plain one-line subjects only; the repo owner is the sole author.

## Coordination

This project uses beads (`bd`) as the single source of coordination. Every
agent reads from and writes to the same bd database. Do not coordinate via
`message_agent` chitchat for anything that could be a ticket — file it.

### What every agent does

- Before starting work, check the queue: `bd ready` for unblocked open
  issues, or run your role's standing query.
- Claim before editing: `bd assign <id> <your-name>` and
  `bd set-state <id> in_progress`. If two agents would touch overlapping
  files, the second defers until the first closes.
- File findings as issues, not chat: `bd create --type bug ...` for defects,
  `--type chore` for cleanup, `--type feature` for additions, `--type epic`
  for multi-issue work, `--type decision` to record a non-obvious choice.
- Link related issues: `--deps blocks:bd-N`, `discovered-from:bd-N`,
  `parent:bd-N` (for epic children).
- Always set `--external-ref` when an issue traces back to a commit or PR
  (e.g., `--external-ref "review:abc123"`).
- Close issues you finish: `bd close <id>`. Never close someone else's work.

### Issue type conventions

- `bug` — something is wrong. P0/P1 reserved for breaks-the-build / data-loss.
- `chore` — cleanup, simplification, dead-code removal, test gaps.
- `feature` — new capability.
- `epic` — multi-issue work; children link with `parent:<epic-id>`.
- `decision` — architectural choice that should be queryable later.

### Branching and integration

- Your team works on one branch, in an **isolated clone** of the project
  mounted at `/workspace`. You commit there. That is the extent of your
  git authority.
- You do **not** push, merge, open pull requests, or acquire merge slots.
  Your container has no remote and no GitHub credentials by design. The
  **host** (outside the sandbox) reviews the approved branch and
  integrates it. Your "done" is a local commit plus the handoff ping your
  role defines.
- Per-clone isolation eliminates concurrent-edit collisions; the host
  serializes the final integration step.

### Context discipline

You are running with a finite context window. Treat your context like a
budget that depletes. The orchestrator tracks your usage and will compact
or restart you, but you have a role to play in keeping yourself useful:

- The orchestrator dispatches `/compact` automatically when your context
  crosses **60%** OR when you hit a natural break (a ticket closes, you
  suspend after handing off). When `/compact` is sent to you,
  cooperate — do not protest, do not summarize differently than `/compact`
  produces. Continue your role from the summary.
- Natural breaks are good moments to stop holding state. After you finish
  a discrete unit (closing a ticket, handing off a commit, sending a
  verdict), let the next pass start clean.
- Do not hoard verbatim tool output in conversation. Cite file:line
  references and let the next read-of-record happen on demand.
- If you find yourself re-reading the same file three times, compact
  yourself proactively (`/compact`). The summary is cheaper than the
  re-read.
- If you are part of a team and your role's work is done for now (e.g.,
  you are the planner and you have filed the spec), call `team_suspend`
  with a reason. The orchestrator will wake you only when needed.

### Messaging

`message_agent` is for transient context the queue should not carry — e.g.,
"I'm picking up bd-42, expect a commit soon" or "blocked on you, see bd-99."
If you would have sent a critique or assignment, file it as a ticket instead.

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

### Non-interactive execution

You are running headless in a container. There is no human at a terminal
waiting to answer prompts. Skills, scripts, or sub-agents you invoke that
include a "confirm with the user" / "ask the user to approve the draft"
step do not apply to you — skip that step and proceed with the action.
The reviewer (and, after integration, the human) is your confirmation
gate, not a pre-action prompt. You never push or open PRs; the host
integrates your approved branch, so there is no host-approval step for
you to wait on during normal work.
