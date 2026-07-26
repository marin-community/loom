-- Staged reviews: creator-private drafts over versioned subjects, submitted
-- atomically into a durable delivery outbox.
CREATE TABLE reviews (
    id                    INTEGER PRIMARY KEY,
    repo_root             TEXT NOT NULL,
    branch_id             TEXT NOT NULL,
    session_id            TEXT NOT NULL,
    subject_kind          TEXT NOT NULL,
    subject_id            TEXT NOT NULL,
    subject_key           TEXT NOT NULL,
    subject_label         TEXT NOT NULL,
    subject_version       TEXT NOT NULL,
    status                TEXT NOT NULL DEFAULT 'draft',
    summary               TEXT NOT NULL DEFAULT '',
    draft_revision        INTEGER NOT NULL DEFAULT 1,
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
    ON reviews(created_by, session_id, subject_kind, subject_id)
    WHERE status = 'draft';
CREATE INDEX idx_reviews_subject
    ON reviews(branch_id, session_id, subject_kind, subject_id, id);

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
    last_error      TEXT,
    lease_token     TEXT
);

-- Artifact ids are immutable review subject identities. SQLite may reuse the
-- largest deleted INTEGER PRIMARY KEY, so allocate envelopes from a durable
-- monotonic sequence seeded after every artifact that predates reviews.
CREATE TABLE artifact_id_sequence (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    next_id   INTEGER NOT NULL
);
INSERT INTO artifact_id_sequence (singleton, next_id)
SELECT 1, COALESCE(MAX(id), 0) + 1 FROM artifacts;
