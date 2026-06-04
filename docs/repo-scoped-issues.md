# Repo-scoped issues

Status: **implemented**. Issues were per-branch (`issues.branch_id →
branches.id`); they are now repo-owned (`issues.repo_root`) with `source_branch`
/ `claimed_branch` annotations. This doc captures the design and the narrower
question that motivated it:

> Should `weaver issue ls` default to **branch-constrained** or **repo-wide**?

One refinement landed during implementation: a branch's working set is
`claimed_branch = B` alone (not `source_branch = B OR claimed_branch = B`).
`source_branch` is pure provenance and stays out of the default filters, so a
backlog item you *authored* doesn't masquerade as your active work.

## TL;DR

1. **Move issue identity to the repo** (`repo_root`), and keep the branch as an
   *annotation* on each issue (`source_branch`, optional `claimed_branch`).
   Storage grain and listing default are **orthogonal** decisions — make the
   model repo-level without forcing every reader to see the whole repo.
2. **Key on `repo_root`, not the GitHub slug.** "Per-GitHub-repo" is the right
   *mental model*; `repo_root` is the right *mechanism*. The slug is sparse
   (only set on `--issue` launches) and absent for local-only repos.
3. **`weaver issue ls` defaults to a two-section view**: the branch's working
   set + the *unclaimed* repo backlog (never other branches' active work, and
   the backlog is capped). The **loom dashboard / `loom` CLI default to
   repo-wide**. Each surface's default matches its caller. Explicit `--repo` /
   `--mine` / `--branch` overrides on the CLI.

The rest of this doc argues for each.

## Why move off the branch at all

The branch-scoped model can't express the fan-out workflow:

> Create N issues up front, then launch one loom session (= branch + worktree)
> per issue.

At creation time **there is no branch yet**. Today those issues would either
fail to belong anywhere or pile onto whatever branch the author happened to be
standing on (often `main`). Issues that *precede* the work that addresses them
are inherently repo-level — this is exactly GitHub's own model (issues belong to
a repo; branches/PRs *reference* them).

Meanwhile the second workflow — an in-worktree agent jotting "things still to do
on *this* branch" — is inherently branch-local. The two workflows pull the
listing default in opposite directions. That tension is the whole question, and
it's resolved by noticing the two workflows have **different primary callers**.

## Data model

```
issue
  id            integer pk
  repo_root     text  not null     -- identity / ownership (canonical primary worktree path)
  github_repo   text                -- denormalized annotation, owner/name, nullable
  source_branch text                -- branch the issue was created from; null if repo-root
  claimed_branch text               -- branch/worktree currently working it; null if unclaimed
  title, body, status, github_issue, created_at, updated_at, closed_at
```

- **`repo_root`** is the canonical primary-worktree path weaver already derives
  in `branch::resolve_from_path` (all worktrees of one clone collapse to one
  `repo_root`). It is 1:1 with a local clone — the natural grain for "the
  repo's backlog." Two independent clones of the same upstream stay separate,
  which is the least-surprising behavior; aggregate by `github_repo` in the UI
  if cross-clone grouping is ever wanted.
- **`source_branch`** answers "where did this come from" (use case B).
- **`claimed_branch`** answers "who is working it" (use case A). `loom launch`
  stamps it when a session picks up an open repo issue; this is what turns a
  repo-level backlog item into a session's working item.

"Branch-scoped" then means `source_branch = me OR claimed_branch = me` — the
agent's working set. "Repo-wide" drops the filter.

### Migration is lossless and backward-compatible

For each existing issue: `repo_root := branch.repo_root`,
`source_branch := branch.branch`, `claimed_branch := branch.branch`
(branches and worktrees are 1:1 today, so the issue is both sourced-from and
claimed-by that branch). After migration the branch-scoped view reproduces
**exactly** today's per-branch list — so defaulting the CLI to branch-scoped is
a true no-op for existing users.

`BranchView.open_issue_count` (the per-session badge) counts
`claimed_branch = branch`, preserving its current meaning. A repo-wide count is
a separate query (`open_count_for_repo`).

## The default: branch vs repo

`weaver issue ls`'s dominant caller is the **in-worktree agent** (see
[primer.md](../crates/weaver-core/primer.md): "the branch's issue list"). Its
mental model and its context budget are branch-local. A repo-wide dump (dozens
of items from unrelated workstreams) is noise that pollutes the agent's context
and invites scope creep ("I see #42 about the logger, let me fix that too" while
on an unrelated branch).

| | Default branch-scoped (recommend, CLI) | Default repo-wide (recommend, dashboard) |
|---|---|---|
| Agent context | Clean — only its own work | Polluted with the whole backlog |
| Use case B (branch todos) | Free, no flags | Needs `--branch`; every agent run pays the filter |
| Use case A (fan-out) | Needs repo-level **create** + a board surface (loom) | Free — the board *is* the backlog |
| Backward compat | No-op after migration | Behavior change for every agent |
| Scope-creep risk | Low | High |
| Surprise ("where did my issues go?") | Mitigated — default also shows the repo backlog (below) | None |

### Why not context-adaptive (scope by where you stand)?

Tempting: branch-scope inside a worktree, repo-scope at the repo root on the
base branch. But the agent is **always** inside a worktree on a non-base
branch, so context-adaptivity collapses to branch-scope for it 100% of the time.
The only caller it would ever help is a human standing on `main` — and that
human is better served by the dashboard or an explicit `--repo`. So the magic
buys ~nothing over "branch default + explicit `--repo`," at the cost of a
less predictable contract for a latency-sensitive, agent-facing tool.

### Surfaces and their defaults

`weaver issue ls` defaults to a **two-section view**: the branch's working set
*plus* the unclaimed repo backlog — "what I'm doing + what's available to pick
up." Relative to the current branch `B`, issues fall in three buckets:

1. **Mine** — `claimed_branch = B`
2. **Unclaimed backlog** — `claimed_branch IS NULL`
3. **Other branches' active work** — `claimed_branch = some other branch`

The default shows **1 + 2**, never 3. Bucket 3 is another agent's in-flight
task — the noisiest, least-relevant, highest-scope-creep set for this agent — so
it stays behind `--repo`. The backlog section is **capped** with an overflow
line, because a fan-out author can seed dozens of issues and an uncapped dump
would blow the agent's context budget on every `ls`:

```
On this branch (2):
  #7  [ ] wire up the retry path
  #9  [ ] backfill old rows
Repo backlog (5 unclaimed, showing 5):
  #11 [ ] schema migration
  ...
  (+0 more — weaver issue ls --repo)
```

- **`weaver issue ls`** (agent CLI): buckets 1 + 2 (open), backlog capped.
  `--repo` → full repo, all three buckets grouped + uncapped. `--mine` →
  bucket 1 only (focused agents / scripts). `--all` composes (adds closed).
  `--branch <key>` retargets "current". Keep the `issue` namespace — no
  top-level `weaver ls` alias.
- **`weaver issue add`**: default attach-to-current-branch (`source_branch = me`)
  so use case B keeps working flag-free; `--repo` creates an unclaimed
  repo-level issue (fan-out authored from inside any branch).
- **loom dashboard / `loom` CLI**: default repo-wide — the board *is* the
  backlog; "launch a session per open issue" stamps `claimed_branch`.

## Lifecycle (resolved)

- **Issues are repo-owned and survive branch/session teardown.** Drop the old
  `branch_id … ON DELETE CASCADE`; a removed branch must not delete its issues.
- **Merging / finishing a session does not auto-close issues.** Closing a
  workstream ≠ resolving the task; `weaver issue close` is the explicit "done".
- **Removing a session (`loom rm`) clears `claimed_branch`** on its issues — the
  issue returns *open* to the unclaimed backlog, ready to be re-picked.
- *Interaction to watch:* an issue worked-but-not-closed before teardown
  reappears in the backlog. That's intended — merge isn't "resolved"; close is.
  The fix is simply to `close` when actually done.
