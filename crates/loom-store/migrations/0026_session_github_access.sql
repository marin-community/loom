CREATE TABLE IF NOT EXISTS session_github_access (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    repository TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('write', 'none')),
    granted_by TEXT NOT NULL,
    granted_at TEXT NOT NULL,
    PRIMARY KEY (session_id, repository)
);
