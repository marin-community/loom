---
name: pull-request
description: Commit cleanly, run proportional gates and lint review, and handle PR/CI work within the authorized scope. Hand delegated candidates and reviews to the immediate parent. Use when committing, pushing, or creating/updating a weaver pull request.
---

# Skill: Pull Request

Clean the branch, commit, apply the lint-review policy, open or update the PR,
then drive CI green and hand it to the designated reviewer. Commit
before any review run — it reads the committed branch diff and only reports.

For delegated work, the immediate parent owns disposition and review repairs.
If the task ends at a candidate commit or review, stop at that authorized scope
and send a typed result on your own channel; do not infer permission to push or
open a PR. Deterministic gates and the lint-review decision still apply.

## Checklist

WIP checkpoint: **1, 2, 4, 5, 7**, when pushing is in scope. A delegated candidate
handoff uses steps **1–6** plus its typed result. Full list before
opening/updating a PR.

1. Self-review the diff.
2. Gate — `./scripts/pre-commit.sh` (Rust + frontend deterministic checks). Not
   the lint review — that's step 6.
3. Tests when warranted — select the relevant layer; use
   `./scripts/test-representative.sh` for broad cross-layer feedback and the
   exhaustive suites when the change's risk warrants them.
4. Stage the specific files.
5. Commit. ← clean checkpoint.
6. Lint-review decision — run `scripts/lint-review.py` when warranted; otherwise
   record why it was skipped.
7. Push.
8. Open or update the PR.
9. Drive CI green, address comments already present, and hand off.

## 1. Self-review

Read your `git diff`. Drop dead code, debug leftovers, stale comments; tighten
names. The review in step 6 reports — it won't clean up for you.

## 2. Gate

```bash
./scripts/pre-commit.sh        # must pass
```

fmt fails → `cargo fmt --all`. Fix clippy by hand — never `#[allow]` past it.
Don't `--no-verify` without a reason.

## 3. Tests (when relevant)

- `./scripts/test-representative.sh` — unit suites plus selected cross-layer
  journeys for broad local feedback.
- `cargo test --workspace` — exhaustive Rust unit + integration coverage when
  backend scope or risk warrants it.
- `cd e2e && npm test` — exhaustive Playwright coverage when UI behavior or a
  route it consumes warrants it.
- `cd python/weaver-loom && uv run pytest` — Python client and watch-program
  logic when that package changes.

Don't disturb the user's live loom — see AGENTS.md.

## 4. Stage

Stage the specific files for this work. No `git add -A`/`.`. Never stage secrets.
Unrelated changes go in a separate commit, not smuggled in.

## 5. Commit

Conventional Commits: `type(scope): summary` — `feat`/`fix`/`docs`/`refactor`/
`chore`, scope is the area (`loom`, `weaver`, `lint`, `config`, `watch`).
Imperative, lower-case, ≤72 chars. The `(#NN)` suffix lands on merge, not from you.

- Body (optional): what changed and why — context the diff lacks. Short.
- Project voice — no `Co-Authored-By: <tool>`, no "Generated with…" trailer, even
  if a harness default suggests one.

Hook fails → fix and commit again.

## 6. Lint-review decision

Run the agent lint review for a substantive initial implementation or a
follow-up that materially changes the design or risk surface. Skip it for a
small, low-risk PR. Once a branch has already had an agent lint review, also
skip small review/CI follow-ups. Use the [canonical detailed
criteria](../../docs/lint.md#when-to-run), including its risk exclusions and
instruction not to decide from raw line count alone.

When a review is warranted, run:

```bash
scripts/lint-review.py         # the agent lint over the branch diff
```

Run after the commit, before the PR. Findings print as `path:line: wl-code
(confidence) message`; ≥0.9 blocks. Fix or answer each, landing fixes in a new
commit. False positive → `// wl-allow: <code>` on the line. Apply findings when
they make the code better, not blindly.

Deeper pass on a big change: `/code-review` (`ultra` = multi-agent cloud). On a
solo PR, read its findings and fix — don't post them to your own PR.

When skipping, add one concise sentence to the PR/testing notes explaining why,
for example: `Agent lint review skipped: documentation-only workflow change
with no runtime behavior.` Do not add a checklist or scoring framework.

## 7. Push

```bash
git push        # -u origin HEAD if no upstream
```

Rebased, or rejected for diverged history → force-push with `--force-with-lease`.

## 8. Open or update the PR

Open when ready. **Never merge or push to `main`.** The body becomes the
squash-merge message — plain text.

- Title: `type(scope): summary`, imperative.
- Body: what changed and why. `Fixes #NN` / `Part of #NN` when a real issue
  exists; don't invent one.

```bash
gh pr create --title "<title>" --body "<plain text body>"
```

Keep title and body matched to the branch's actual scope, including when updating
a branch that already has a PR.

**Hard rules:**

- Body is *what & why* — no "Testing"/"Validation" section, no "written by…".
  A single unheaded testing sentence is allowed when recording why the agent
  lint review was skipped.
- No checkboxes, no emoji, no filler openers ("This PR…", "Summary of changes:").
- ≤500 words; Markdown sections only when a large change needs them.
- No self-credit.

## 9. Drive CI green and hand off

Opening the PR starts this step. **Local green ≠ CI green:** CI runs more than the
local gate (Playwright `e2e/`, CodeQL, clean-checkout SPA build). Stay through
CI, then hand off to the coordinator/human final reviewer. Do not merge.

Block on CI, don't re-poll:

```bash
gh pr checks <N> --watch --fail-fast
```

Failure → read the job log and fix it. A failure in a file you didn't touch
isn't automatically pre-existing — confirm it fails on `main` without your
change first. Never silently absorb a failure.

Once CI is green, address any comments already present. Fix clear comments in a
new commit and reply in-thread. A delegated session routes unclear comments or
agent-owned blockers to its immediate parent as a typed `parent_help` result,
keeping status `ok`. Do not poll for future reviews or wait for them.

Keep status `ok` while CI runs. Once CI is green and comments already present
are handled, hand off the verified scope. Delegated `ready-for-review`,
`approval`, `request-changes`, `failedchecks`, and `context` outcomes remain
`ok`. Send `loom channels send --kind result "<outcome, commit/PR, checks, limits,
and follow-up>"` on your own channel to wake the immediate parent; status alone
or ending the turn does not wake it. The parent owns review follow-ups and
repairs. Escalate through the agent hierarchy; reserve `attention`/`blocked` for
a genuinely unresolved human decision or access requirement. Do not ask the user
to manage agents or their repairs.
Human review requests identify the concrete unresolved decision and reviewable
resources; they are distinct from delegated parent review.

Before ending a delegated turn, verify your open own-channel delivery binding
and parentage, save state, and publish the typed result. If waiting on children,
end the turn after saving state rather than busy polling. A completed child turn
or local green does not establish integration readiness.
Close the tracking issue when the PR is open and the work is genuinely done —
not before.

## Rules

- `./scripts/pre-commit.sh` is the gate; make the lint-review decision after
  committing.
- Never merge or push to `main`; open a PR when it is in the authorized scope.
- Force-push with `--force-with-lease`, e.g. after a rebase.
- No self-attribution in commits or PR bodies.
- Nothing to commit → say so, stop.
- AGENTS.md — the rest of the hacking guide (build/test internals, live-loom
  caution, conventions).
