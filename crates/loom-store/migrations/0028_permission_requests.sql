CREATE TABLE IF NOT EXISTS session_permission_requests (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('github_repository')),
    repository TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('write')),
    reason TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'approved', 'denied')),
    requested_by TEXT NOT NULL,
    requested_at TEXT NOT NULL,
    decided_by TEXT,
    decided_at TEXT,
    decision_reason TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_permission_requests_one_pending_scope
    ON session_permission_requests(session_id, kind, repository, mode)
    WHERE state = 'pending';
