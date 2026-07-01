-- Discussion: stand-off comment threads on an artifact span, resolvable.
CREATE TABLE artifact_threads (
    id            INTEGER PRIMARY KEY,
    artifact_id   INTEGER NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    base_rev      INTEGER NOT NULL,             -- artifact rev the anchor was taken from
    anchor_quote  TEXT NOT NULL DEFAULT '',     -- the selected text (primary selector)
    anchor_prefix TEXT NOT NULL DEFAULT '',     -- a little context before (disambiguation)
    anchor_suffix TEXT NOT NULL DEFAULT '',     -- a little context after
    status        TEXT NOT NULL DEFAULT 'open', -- 'open' | 'resolved' | 'orphaned'
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    resolved_at   TEXT                          -- set when status -> resolved
);
CREATE INDEX idx_threads_artifact ON artifact_threads(artifact_id);

CREATE TABLE artifact_comments (
    thread_id  INTEGER NOT NULL REFERENCES artifact_threads(id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,                -- 1-based, per thread
    author     TEXT NOT NULL,                   -- 'agent' | 'user'
    body       TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (thread_id, seq)
);
