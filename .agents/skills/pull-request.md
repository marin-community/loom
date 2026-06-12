---
name: pull-request
description: Commit cleanly, run the gate, hand off to the agent lint review, open or update the PR, then monitor it and answer comments until it merges. Use when committing, pushing, or creating/updating a weaver pull request.
---

# Skill: Pull Request

Clean the branch, commit, run the lint review, open or update the PR, then stay
with it until it merges. Commit before the review — it reads the committed branch
diff and only reports.

Weaver is solo: skip team ceremony, but the gate, the lint review, and driving CI
green are not optional.

## Checklist

WIP checkpoint: **1, 2, 4, 5, 7**, stop. Full list before opening/updating a PR.

1. Self-review the diff.
2. Gate — `./scripts/pre-commit.sh` (fmt + clippy + staged lint).
3. Tests when warranted — `cargo test --workspace`; `cd e2e && npm test` for UI.
4. Stage the specific files.
5. Commit. ← clean checkpoint.
6. Lint review — `scripts/lint-review.py`; fix or answer every finding.
7. Push.
8. Open or update the PR.
9. Monitor — drive CI green, answer every comment, in a loop.

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

- `cargo test --workspace` — backend unit + integration (needs git; spawns tapestry PTYs).
- `cd e2e && npm test` — Playwright UI, when you touched the SPA or a route it hits.

Don't disturb the user's live loom — see AGENTS.md.

## 4. Stage

Stage the specific files for this work. No `git add -A`/`.`. Never stage secrets.
Unrelated changes go in a separate commit, not smuggled in.

## 5. Commit

Conventional Commits: `type(scope): summary` — `feat`/`fix`/`docs`/`refactor`/
`chore`, scope is the area (`loom`, `weaver`, `lint`, `config`, `overlooker`).
Imperative, lower-case, ≤72 chars. The `(#NN)` suffix lands on merge, not from you.

- Body (optional): what changed and why — context the diff lacks. Short.
- Project voice — no `Co-Authored-By: <tool>`, no "Generated with…" trailer, even
  if a harness default suggests one.

Hook fails → fix and commit again.

## 6. Lint review

```bash
scripts/lint-review.py         # the agent lint over the branch diff
```

Run after the commit, before the PR. Findings print as `path:line: wl-code
(confidence) message`; ≥0.9 blocks. Fix or answer each, landing fixes in a new
commit. False positive → `// wl-allow: <code>` on the line. Apply findings when
they make the code better, not blindly.

Deeper pass on a big change: `/code-review` (`ultra` = multi-agent cloud). On a
solo PR, read its findings and fix — don't post them to your own PR.

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
- No checkboxes, no emoji, no filler openers ("This PR…", "Summary of changes:").
- ≤500 words; Markdown sections only when a large change needs them.
- No self-credit.

## 9. Monitor — in a loop

Opening the PR starts this step. **Local green ≠ CI green:** CI runs more than the
local gate (Playwright `e2e/`, CodeQL, clean-checkout SPA build). Stay until it
merges (or the user says stop). A summary message is not an exit condition.

Block on CI, don't re-poll:

```bash
gh pr checks <N> --watch --fail-fast
```

Green → poll comments/reviews on a backoff (`ScheduleWakeup` 270s, doubling, give
up after a few idle hours). Each pass check **both**:

1. CI — `gh pr checks <N>`. Failure → read the job log, fix it. A failure in a
   file you didn't touch isn't automatically pre-existing — confirm it fails on
   `main` without your change first; if your change caused it, it's your
   regression. Never silently absorb a failure.
2. Comments/reviews — `gh pr view <N> --json reviews,comments` and `gh api
   repos/<owner>/<repo>/pulls/<N>/comments`. Green CI ≠ nothing to do; people and
   bots comment after CI passes.

Answer every comment: fix the clear ones (commit as a **new** commit, reply
in-thread prefixed 🤖, resolve). Genuinely unclear → `weaver status attention
"<question>"`, keep monitoring while you wait.

Status tracks the loop: `ok` while CI runs or you await review; `weaver
status attention "ready for review"` only once green and every comment is
handled. Close the tracking issue when the PR is open and the work is genuinely
done — not before.

## Rules

- `./scripts/pre-commit.sh` is the gate; commit before `lint-review.py`.
- Never merge or push to `main`; open a PR.
- Force-push with `--force-with-lease`, e.g. after a rebase.
- No self-attribution in commits or PR bodies.
- Nothing to commit → say so, stop.
- AGENTS.md — the rest of the hacking guide (build/test internals, live-loom
  caution, conventions).
