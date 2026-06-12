-- Artifacts: named, versioned documents an agent (or the user) writes *to
-- weaver*, not to the repo. An artifact belongs to a repo and optionally a
-- branch (NULL = repo-shared); every write appends an immutable revision.
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

-- SQLite UNIQUE treats NULLs as distinct, so the UNIQUE above does not guard
-- repo-shared names (branch_id IS NULL); this partial index does.
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
