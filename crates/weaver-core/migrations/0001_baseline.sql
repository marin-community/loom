-- Baseline schema: the shape weaver carried before the migration framework
-- existed. On a brand-new database this creates everything; on a database that
-- predates the framework every statement is a harmless no-op (the tables are
-- already there), and the migration is simply recorded as applied. Subsequent
-- changes are expressed as their own numbered migration rather than edited in
-- here, so this file stays a faithful record of the starting point.

-- A branch the agent is working on. Identified by `(repo_root, branch)`; the
-- 8-char `id` is internal — agents never see it.
CREATE TABLE IF NOT EXISTS branches (
    id           TEXT PRIMARY KEY,
    repo_root    TEXT NOT NULL,
    branch       TEXT NOT NULL,
    base_branch  TEXT NOT NULL DEFAULT 'main',
    goal         TEXT NOT NULL DEFAULT '',
    title        TEXT NOT NULL DEFAULT '',
    -- The agent's current-state message, set together with `attention` via
    -- `weaver set-status` ("Wired up routes; tests pass"). The trail of these
    -- (recorded as `attention` events) is the branch's progress history.
    description  TEXT NOT NULL DEFAULT '',
    -- Agent-declared attention: an urgency level (ok | attention | blocked) —
    -- the agent's own signal of whether it needs the user, set via
    -- `weaver set-status`. The reason rides in `description`.
    attention    TEXT NOT NULL DEFAULT 'ok',
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE(repo_root, branch)
);

-- Issues belong to a **repo** (`repo_root`), not a branch. The branch they were
-- created from (`source_branch`) and the branch currently working them
-- (`claimed_branch`) are annotations: `claimed_branch IS NULL` is the unclaimed
-- repo backlog. Repo-owned means an issue outlives the branch/worktree that
-- spawned it — see docs/repo-scoped-issues.md.
CREATE TABLE IF NOT EXISTS issues (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_root      TEXT NOT NULL,
    github_repo    TEXT,
    source_branch  TEXT,
    claimed_branch TEXT,
    title          TEXT NOT NULL,
    body           TEXT NOT NULL DEFAULT '',
    status         TEXT NOT NULL DEFAULT 'open',
    github_issue   INTEGER,
    -- Link to a plan task, `"<slug>#T3"`, when this issue was materialized from
    -- a plan (docs/structured-projects.md). NULL for ordinary issues.
    plan_task      TEXT,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    closed_at      TEXT
);
CREATE INDEX IF NOT EXISTS idx_issues_repo ON issues(repo_root, status);
CREATE INDEX IF NOT EXISTS idx_issues_claimed ON issues(repo_root, claimed_branch);

-- Free-form progress notes appended by `weaver note`. Dropped in 0002 — the
-- status-description trail (`attention` events) subsumes them — but recreated
-- here so the baseline is the honest pre-framework schema.
CREATE TABLE IF NOT EXISTS notes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    branch_id   TEXT NOT NULL REFERENCES branches(id) ON DELETE CASCADE,
    text        TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE IF NOT EXISTS events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    branch_id   TEXT NOT NULL,
    kind        TEXT NOT NULL,
    data        TEXT NOT NULL DEFAULT '{}',
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_events_branch ON events(branch_id, id);

CREATE TABLE IF NOT EXISTS settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
