# Artifacts: agent-authored documents, out of the repo

Status: **proposal** (weaver issue #117). This supersedes the task-sync half of
[structured-projects.md](structured-projects.md): the projection principle it
established — *structure in a document, state in the DB, the render joins
them* — survives and generalizes; the plan noun, the slug rules, and the
reconcile engine built around it do not.

## The problem

The `plan` feature bundles three jobs into one repo-committed file:

1. **Telling agents what to do** — the design surface, the spec.
2. **Expressing a set of steps and syncing them to weaver** — task headings
   with stable IDs, materialized into issues by `weaver plan sync`.
3. **Showing structured content to the user** — rendered markdown, mermaid,
   live status badges on the dashboard.

The bundle leaks everywhere the jobs pull apart:

- **A plan must be committed to exist.** It is a worktree file, so every
  "let me sketch this for you" produces a document in the user's repo that
  nobody wants to check in. The ecosystem has codified the opposite
  convention — agent scratch is gitignored, only instructions are committed
  ([Agents.gitignore](https://github.com/github/gitignore/blob/main/Global/Agents.gitignore));
  aider, which writes its history files into the working tree, is the
  standing cautionary tale.
- **A plan dies with its worktree.** Archiving a session preserves the goal,
  status trail, events, and issues — everything except the one document that
  explains them, unless it was committed (see above).
- **Goal and plan say the same thing twice.** `plan new` copies the branch
  goal into the file's "Problem & goal" section once
  (`crates/weaver-core/src/plan.rs` scaffold); after that the DB goal and the
  file
  drift with no link in either direction. Agents are routinely unsure which
  one to update — the confusion that filed #117.
- **The machinery tax is high for an opt-in feature.** Repo-wide slug
  identity, branch-scoping subtleties, in-flight flag rules, a reconcile
  verb, `.weaver/config.toml [plan].dir` — all to keep one markdown file and
  one SQLite table agreeing.
- **The sync engine is a worse agent.** `plan sync` computes a literal diff
  (create / close / retitle / flag). But every weaver session has a resident
  agent that, told "make the issues match the plan", applies judgment about
  in-flight work, partial overlaps, and renames. We built a dumb reconciler
  next to a smart one.

## What the field settled

A survey of agent products, protocols, and doc↔tracker systems (June 2026)
points one direction. The decisive datapoints:

- **GitHub ran our experiment and walked it back.** Tasklist blocks —
  markdown-encoded task hierarchy synced to issue relations — ran ~2.5 years
  in beta, never GA'd, and were retired April 2025 in favor of DB-native
  sub-issues, pitched verbatim as tracking work "without relying on
  Markdown". What survived is the classic task list: the doc *references*
  issues (`- [ ] #123`), the tracker owns state, the render unfurls it, and
  closing the issue checks the box — two one-way flows, no reconciliation.
- **Render-time projection is the architecture that survived everywhere.**
  Confluence's Jira macro (the page stores a query handle; Jira owns state;
  view time joins them) has run twenty years with failure modes that are
  only operational (latency, caching) — never divergence. Notion's tracker
  integrations are deliberately one-way and read-only. GitLab `#123+` refs
  render live state. Org-mode and Obsidian — doc-as-database designs — break
  exactly at concurrent writers, which weaver has by construction (agent,
  user, sub-agents).
- **Every hosted agent stores deliverables platform-side, not in the
  repo.** Devin's plans/playbooks/wikis, Amp's URL-addressable threads,
  Claude's artifacts (immutable versions + a picker + a publish URL),
  Manus's library (share the artifacts, never the sandbox), Cursor's agent
  to-dos. Repo output is commits and PRs, full stop.
- **But the export hatch is also blessed.** Factory's Specification Mode
  saves an approved spec to a configurable repo path; Ultraplan's cancel
  path saves the plan to a file; OpenAI tells Codex users to keep `plan.md`
  in-repo precisely because Codex *lacks* an artifact store. Out-of-repo by
  default, committable on request.
- **A2A's artifact model:** named, MIME-typed, versioned by snapshot — and
  the *client* owns version lineage, not the agent. MCP's `resource_link`
  shows the complement: a tool result that returns a URI handle another
  agent can fetch later.

Full survey with citations: [Sources](#sources).

## The proposal

One new noun, one demotion, one deletion — and a thin shared layer:

| Job the plan tab did | New home |
|---|---|
| Tell agents what to do | **goal** — mutable markdown on the branch; `weaver goal set <file>` |
| Steps synced to weaver | **issues** — the only task ledger; created directly, never parsed out of a doc |
| Structured content for the user | **artifacts** — named, versioned documents in weaver.db, rendered by loom |

**smartdoc** is the read-side glue: the markdown-convention layer (parse
references, probe live status, project it into the render) that lets each of
the three point at the others without duplicating their facts.

### Artifacts: the new noun

An artifact is a document an agent (or the user) writes *to weaver*, not to
the repo: a design, a report, a diagram, a plan. Properties, each earned by
the survey above:

- **Named and scoped like issues.** An artifact belongs to a repo and
  optionally to a branch — the same shape issues already have
  (claimed vs repo backlog). `weaver artifact write plan design.md` scopes
  to the current branch; `--repo` publishes it repo-shared (the fan-out
  case: one plan, many child sessions). Listings show branch-scoped names
  prefixed by their branch, which is the "prefixed with the session ID"
  behavior as a display rule rather than a string convention.
- **Versioned by snapshot, never mutated.** Every write appends an immutable
  revision; the viewer gets a version picker; "latest" is the default read.
  This is the Claude-artifacts / A2A model, and it makes concurrent
  agent/user edits safe — last write is a new revision, not a lost update.
- **Kind-typed, markdown-first.** `kind` defaults to `markdown` (GFM +
  mermaid via the existing `MarkdownView`); other kinds render as source
  until they earn a renderer. Binary kinds (screenshots) are a deliberate
  v2.
- **URL-addressable.** `weaver artifact write` prints the dashboard URL
  (`/s/<session>/artifacts/<name>`) so the agent can hand it to the user in
  a status message or PR comment — the Amp-thread lesson: the URL is the
  collaboration feature. The CLI resolves the server address the way loom
  clients already do (`$WEAVER_API`, then the recorded `server.json`
  address); without a running loom the write still succeeds — it is a plain
  DB write, daemon-less like everything else — and the CLI prints the name.
- **It survives archive.** Artifacts live in weaver.db next to the goal,
  events, and issues, so tearing down the worktree no longer deletes the
  design doc. This fixes the worst current asymmetry for free.

Storage — two tables, content in the DB (documents are kilobytes; this is a
hundreds-of-rows database):

```sql
CREATE TABLE artifacts (
    id          INTEGER PRIMARY KEY,
    repo_root   TEXT NOT NULL,
    branch_id   TEXT REFERENCES branches(id) ON DELETE CASCADE, -- NULL = repo-shared
    name        TEXT NOT NULL,
    kind        TEXT NOT NULL DEFAULT 'markdown',
    title       TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE(repo_root, branch_id, name)
);
-- SQLite UNIQUE treats NULLs as distinct; repo-shared names need their own guard:
CREATE UNIQUE INDEX idx_artifacts_shared ON artifacts(repo_root, name)
    WHERE branch_id IS NULL;

CREATE TABLE artifact_versions (
    artifact_id INTEGER NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    rev         INTEGER NOT NULL,
    author      TEXT NOT NULL DEFAULT '',     -- 'agent' | 'user'
    content     TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (artifact_id, rev)
);
```

CLI and API, in the house idiom:

```
weaver artifact write <name> [<file>]    # stdin with '-'; --title, --kind, --repo
weaver artifact ls [--repo]              # this branch's + shared; --repo for all
weaver artifact show <name> [--rev N]    # content; --meta for the envelope

GET  /api/sessions/{id}/artifacts                 # list: branch-scoped + shared
GET  /api/sessions/{id}/artifacts/{name}?rev=N    # content + projected refs (below)
PUT  /api/sessions/{id}/artifacts/{name}          # user edit -> new revision
```

Each write records an `artifact_written` event (`{name, rev, title}`) through
the existing bus, so the SSE stream, the activity feed, and overlookers see
it with no new plumbing.

Artifacts are the outbound twin of `scratch/`: scratch is material the user
hands the agent; artifacts are documents the agent hands the user. Scratch
stays as-is.

### Goal: settable from a file, not auto-artifacted

`weaver goal set <file|->` reads a file (or stdin) into `branches.goal`. The
real win is mundane: long markdown goals stop being a shell-quoting exercise,
so agents can actually maintain the goal as understanding evolves. The
Overview renders the goal through the same markdown pipeline as artifacts,
projection included, so a goal can say `the breakdown is #41 #42 #43, design
in [the plan](artifact:plan)` and stay live.

Issue #117 floated auto-creating an artifact on `goal set` and linking it.
Recommendation: **don't.** That puts the same text in two places — the
disease this redesign exists to cure. The goal column already has a surface
(Overview, `weaver goal`, `summary`), a history (`goal_set` events carry the
full text), and a single owner. When the goal genuinely is a document, the
agent writes the artifact and sets the goal to one line plus a reference —
composition instead of coupling.

### Plans: a convention, not a noun

A plan becomes *an artifact named `plan`* that follows smartdoc conventions —
prose, a mermaid architecture diagram, and a task list whose items
**reference issues** instead of declaring tasks:

```markdown
## Tasks
- #41 Index layer — storage + read path
- #42 Executor — depends on #41
- [ ] decide single-node vs distributed (open question, not yet an issue)
```

The doc never states status; the renderer asks the issue ledger at view time
and stamps each `#41` with a live chip (open / claimed-by-branch / closed) —
exactly the GitHub-task-list / Confluence-macro shape, the one that survived
two decades while every doc-as-database design died. There is **no sync
engine**: the agent that writes the plan also files the issues
(`weaver issue add --repo …`) and edits the doc to reference them. When the
user changes the plan mid-flight, they tell the agent — which adjusts the
ledger with judgment (close the unstarted, *talk about* the in-flight)
instead of receiving a flag table. The agent is the reconciler; the smartdoc
skill is its instructions.

What this deletes from today's implementation: the `T1` task grammar with
`exec:`/`value:`/`deps:` annotations, repo-wide slug identity, the
`plan sync` diff/apply engine and its flag rules, `issues.plan_task`, the
`/plan` routes, and `[plan].dir` repo config. If a fan-out ever needs
machine-readable ordering or value, `issue_tags` (already shipped) can carry
`deps`/`value` per issue — closer to where GitHub ended up (relations on the
DB rows, not in the markdown). The auto-generated dependency graph goes too:
the agent authors mermaid directly, which it does better than a renderer
guessing from `deps:` lines.

Committed design docs remain a normal thing — written as ordinary repo files
riding ordinary PRs, like this one. weaver just stops being their manager.
For users who want both, the export hatch (Factory / Ultraplan precedent) is
one flag away: `weaver artifact show plan > docs/plans/x.md`, or a future
`artifact export`.

### smartdoc: the projection layer

A new `crates/smartdoc`, dependency-free of weaver, owning the conventions:

```rust
pub struct Doc { /* parsed blocks */ }
pub enum Ref { Issue(u64), Artifact(String), Session(String) }

pub fn parse(src: &str) -> Doc;                 // extract refs, checklists, frontmatter
pub fn refs(doc: &Doc) -> Vec<Ref>;
pub fn project(doc: &Doc, status: &HashMap<Ref, RefStatus>) -> Projection;
```

weaver-core implements the probes (`Ref::Issue` → the issues table, etc.);
the artifact `GET` returns content *plus the resolved ref map*, so the SPA
chips and a terminal `weaver artifact show` render the same projection —
API-first, no browser-only logic. The generic framing is the point of the
crate boundary (doc conventions wired to pluggable status probes), but v1
honestly has one wiring: weaver. Keep it thin until a second consumer exists.

"Actions" — the write-side of #117's smartdoc sketch — are deliberately
deferred to one verb: **promote**, turning a bare checklist line into an
issue and rewriting the line to reference it (GitHub's surviving doc→DB
flow, Linear's create-from-selection). It ships after projection proves out,
if agents don't simply do it themselves.

### The skill, and WEAVER.md

A `smartdoc` skill (`.agents/skills/`) plus a rewritten WEAVER.md section
replace the plan instructions. The content, tersely: when to write an
artifact (design loops, reports, anything the user should *read* rather than
*run*); the reference conventions (`#N`, `artifact:<name>`, mermaid); the
division of labor (goal = charter, issues = ledger, artifacts = documents);
and the maintenance duty — *you are the sync engine: when the plan changes,
update the issues; when issues change shape, update the doc's references.*

### Loom UI

- **Session detail** gains an Artifacts surface: list + viewer
  (`MarkdownView`), version picker, the file browser's proven
  preview ⇄ Monaco toggle for user edits (each save = new revision,
  `author: user`). Deep link `/s/:id/artifacts/:name`.
- **Overview** pins the well-known `plan` artifact where `SessionPlan`
  renders today; the goal renders as projected markdown above it.
  `SessionPlan.vue`'s reconcile modal goes; its render path becomes the
  artifact viewer.
- **Projection pass** in the markdown renderer: `#123` → live status chip
  linking to the issue, `artifact:` links resolve, checked state for a
  referenced issue comes from the ledger, never the text.
- `artifact_written` joins the activity feed and SSE-driven refresh.

## Lifecycle walkthrough

1. `loom session launch "Plan the search rewrite"`.
2. The agent drafts, then `weaver artifact write plan design.md --title
   "Search rewrite"` → prints `http://…/s/ab12cd34/artifacts/plan`; sets
   `attention "plan ready — see artifact"`.
3. The user reads it rendered (mermaid and all), edits in Monaco (rev 2) or
   replies; the agent revises (rev 3).
4. On blessing, the agent files the breakdown — `weaver issue add --repo
   "Index layer"` … — and rewrites the plan's task section to `- #41 …`,
   republishing `--repo` so children share it.
5. Fan-out: `loom session launch --claim 41` per task, unchanged.
6. The dashboard projects each `#41` live; the doc never changes as work
   proceeds. Mid-flight scope change = user tells the agent; the agent
   adjusts issues with judgment. No reconcile engine, no flags.
7. Archive: the plan and its revisions survive in weaver.db with the rest of
   the branch's history.

Small tasks stay goal-plus-issues, as today — an artifact is one command,
not a ceremony, so right-sizing takes care of itself.

## Delivery

Each step shippable alone:

1. **Tables + CLI + events** — `artifacts`/`artifact_versions`, `weaver
   artifact write|ls|show`, `artifact_written`. Useful immediately as a
   durable, out-of-repo doc store.
2. **Loom surface** — routes, viewer, Overview pin, activity/SSE.
3. **smartdoc + projection** — the crate, ref resolution in `GET`, status
   chips in `MarkdownView`, `weaver goal set`.
4. **The deletion** — retire `weaver plan`, the sync engine, `/plan` routes,
   `plan_task` (migration drops the column), `[plan].dir`; rewrite WEAVER.md
   and ship the skill; mark structured-projects.md superseded. Pre-launch,
   no deprecation window.

## Open questions

- **Binary kinds** (screenshots, the Cursor "demo artifact" pattern): needs
  blob storage and a raw route; v2, the seam (`kind`) is reserved.
- **Cross-branch reads in the CLI**: `artifact ls --repo` covers discovery;
  is a branch-qualified `show` needed, or do shared artifacts cover fan-out?
- **Promote** timing, per above.
- **Retention**: keep forever (rows are tiny, archive already preserves) —
  stated as policy so ephemerality is never an accident.

## Sources

- GitHub tasklist-blocks retirement ([changelog](https://github.blog/changelog/2025-02-18-github-issues-projects-february-18th-update/)),
  [task lists](https://docs.github.com/en/issues/tracking-your-work-with-issues/about-task-lists),
  [sub-issues](https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/adding-sub-issues)
- [Confluence Jira issues macro](https://confluence.atlassian.com/doc/jira-issues-macro-139380.html);
  [Notion synced databases](https://www.notion.com/help/synced-databases)
- Claude [artifacts](https://support.claude.com/en/articles/9487310-what-are-artifacts-and-how-do-i-use-them) and
  [Ultraplan](https://code.claude.com/docs/en/ultraplan);
  [Amp threads](https://ampcode.com/manual);
  [Devin interactive planning](https://docs.devin.ai/work-with-devin/interactive-planning);
  [Jules plan API](https://jules.google/docs/api/reference/sessions/);
  [Factory Specification Mode](https://docs.factory.ai/cli/user-guides/specification-mode);
  [Manus sandbox/sharing](https://manus.im/blog/manus-sandbox);
  [Cursor cloud agents](https://cursor.com/docs/cloud-agent)
- [A2A artifacts](https://a2a-protocol.org/latest/specification/);
  [MCP resources](https://modelcontextprotocol.io/specification/2025-06-18/server/resources)
- [Agents.gitignore](https://github.com/github/gitignore/blob/main/Global/Agents.gitignore);
  org-sync's own caveat ([README](https://github.com/arbox/org-sync));
  Graphite's refs→SQLite migration ([git as KV](https://graphite.com/blog/git-key-value))
