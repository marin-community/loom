# Holistic pull-request reviews

[The workflow](../.github/workflows/holistic-review.yml) launches a read-only
Loom review of each non-draft, same-repository PR opened, reopened, marked ready,
or updated by an author with repository write access. It also accepts a
standalone `/holistic-review` issue comment on a PR from a user with write
access. The manual command works on draft PRs and requests a fresh review even
when the current head was reviewed before. Fork PRs are outside this launcher
because a head commit in another repository is not a reliable base for Loom's
managed checkout. The workflow uses `pull_request_target` and checks out only
its trusted default-branch actions; the PR code is read by the separate review
session at the pinned head SHA, never executed by the Actions runner.

The workflow is advisory, not a required merge check. Automatic runs are
idempotent per PR head, while manual runs are idempotent per comment. A stable
per-PR Loom channel routes later requests into a live review session when one
exists.
An earlier run must verify that its requested SHA is still the PR head before
posting; a later push can make an already launched session stale.

## Review contract

The reviewer reads the title, body, complete issue-comment thread, inline
comments, and submitted reviews. Confirm that both the checkout and current
GitHub PR head match the requested SHA. If either differs, post nothing and
report the stale run. Read the repository's `AGENTS.md`, any instructions that
scope changed paths, the whole diff, and the code around it. Trace changed
interfaces into callers, inspect prior behavior at the merge base, read related
issues, and check open PRs that touch the same files. For changed tests, run
safe affected tests when practical and reason through any skipped cases.

Assess the whole change against these questions:

- Does it fit existing concepts and module boundaries? Is there a smaller
  abstraction that removes surface area?
- Do changed defaults, state transitions, errors, credentials, or lifecycle
  paths have consequences outside the diff?
- Do tests fail when the intended behavior is wrong, including hidden or
  skipped tests? Do the PR's claims match the evidence?
- Does an overlapping PR require a merge order or a change in this PR? Does
  the title and body describe the actual scope?

Report only introduced, actionable defects backed by a file and line, a
concrete consequence, and a concrete replacement. Do not report style
preferences, speculative failures, or pre-existing problems. Summarize the
before and after flow and give the review decision in an overview that stands
alone. Use a Mermaid diagram when it clarifies a cross-layer change. Keep
findings numbered and label each `Blocker` or `Should fix`; include test and
related-PR findings when they need action. State what was verified and what
could not be verified.

The session may create or edit review comments only. It must not edit files,
commit, push, change labels, or open a PR. Every new GitHub comment starts with
`🤖`. Finish with a concise typed result in the session's Loom channel.

## Re-review and posting

Use the earliest issue comment authored by `loom-oa-dev[bot]` with a well-formed
`<!-- marin-holistic-review:<40-hex-sha> -->` marker as the overview. Fetch all
issue comments with pagination; ignore identical markers by other authors.
Read any later bot overviews to recover the latest reviewed SHA and findings,
but update the original comment by its exact comment ID. Do not create a new
overview when editing fails.

For an automatic run, leave comments unchanged when the existing overview
already names the requested head, or when the delta from the previous reviewed
head is under 100 changed lines, changes only docs, generated files, or
fixtures, or consists solely of commits whose subject starts with `Deslop`.
If the previous SHA is no longer on the PR branch, do a full review. A manual
request bypasses these delta and same-head skips. Before *every* write,
re-read the PR head and stop if it differs from the requested SHA.

On a re-review, preserve finding numbers and mark earlier items as open,
changed, or resolved. New items get numbers above the previous maximum. Replace
the original overview with the current walkthrough, findings, and one marker
for the requested head. Optionally post one new top-level comment containing
only new or materially changed actionable findings, with a link to the
overview and a `<!-- marin-holistic-followup:<sha> -->` marker. Never post
inline comments or repeat unchanged findings in the follow-up. Check for an
existing bot-authored follow-up marker for the same SHA before posting one.

## Deployment setup

The workflow uses the same repository Actions variables as
[`loom-issue.yml`](../.github/workflows/loom-issue.yml): `LOOM_URL` (without a
trailing slash) and `LOOM_PROFILE` (default `github-actions`). The selected
profile needs repository read and GitHub comment access. The stock restricted
`github_comment` profile cannot run a holistic review because its fixed tools
do not expose the PR diff, comment history, submitted reviews, or editing an
existing overview comment. Configure a GitHub App installation for the repo.
Grant this new workflow's OIDC identity access to the existing profile, using
the repository's numeric ID and exact workflow ref. For this repository:

```sh
loom federation add loom-holistic-review \
  --audience "$LOOM_URL" \
  --repository-id 1129401356 \
  --workflow-ref 'marin-community/loom/.github/workflows/holistic-review.yml@refs/heads/main' \
  --profile "${LOOM_PROFILE:-github-actions}"
```

The federation is deployment policy, so merging a repository workflow does
not create its mapping. [Restricted GitHub sessions](restricted-sessions.md)
explains OIDC federation and profile capability limits. Audit the mapping with
`loom federation ls` and request one review using a standalone
`/holistic-review` PR comment after setup.
