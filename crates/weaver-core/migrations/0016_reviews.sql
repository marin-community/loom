-- Staged reviews: creator-private drafts over versioned subjects, submitted
-- atomically into a durable delivery outbox.
CREATE TABLE reviews (
    id                    INTEGER PRIMARY KEY,
    repo_root             TEXT NOT NULL,
    branch_id             TEXT NOT NULL,
    session_id            TEXT NOT NULL,
    subject_kind          TEXT NOT NULL,
    subject_key           TEXT NOT NULL,
    subject_label         TEXT NOT NULL,
    subject_version       TEXT NOT NULL,
    status                TEXT NOT NULL DEFAULT 'draft',
    summary               TEXT NOT NULL DEFAULT '',
    created_by            TEXT NOT NULL,
    acknowledged_outdated INTEGER NOT NULL DEFAULT 0,
    delivery_state        TEXT NOT NULL DEFAULT 'draft',
    delivery_error        TEXT,
    delivery_key          TEXT NOT NULL UNIQUE,
    created_at            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    submitted_at          TEXT
);

CREATE UNIQUE INDEX idx_reviews_one_draft
    ON reviews(created_by, session_id, subject_kind, subject_key)
    WHERE status = 'draft';
CREATE INDEX idx_reviews_subject
    ON reviews(branch_id, session_id, subject_kind, subject_key, id);

CREATE TABLE review_comments (
    id              INTEGER PRIMARY KEY,
    review_id       INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    subject_version TEXT NOT NULL,
    anchor_kind     TEXT NOT NULL,
    anchor_json     TEXT NOT NULL,
    body            TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending',
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX idx_review_comments_review ON review_comments(review_id, id);

CREATE TABLE review_delivery_outbox (
    review_id       INTEGER PRIMARY KEY REFERENCES reviews(id) ON DELETE CASCADE,
    delivery_key    TEXT NOT NULL UNIQUE,
    state           TEXT NOT NULL DEFAULT 'queued',
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    last_error      TEXT
);

-- A durable receipt for the conversation queue boundary. A worker retry after
-- a crash can observe this key and mark the outbox delivered without appending
-- the same structured feedback to sessions.pending_prompt twice.
CREATE TABLE review_prompt_deliveries (
    delivery_key TEXT PRIMARY KEY,
    session_id   TEXT NOT NULL,
    enqueued_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
