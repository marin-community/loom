-- Correct the staged-review contract before it ships: canonical public subject
-- keys, optimistic draft revisions, and fenced delivery leases.
ALTER TABLE reviews ADD COLUMN subject_id TEXT NOT NULL DEFAULT '';
UPDATE reviews
SET subject_id = subject_key,
    subject_key = subject_label
WHERE subject_kind = 'artifact' AND subject_id = '';

-- Artifact ids are public immutable identities now. SQLite may reuse the
-- largest deleted INTEGER PRIMARY KEY, so allocate new envelopes from a
-- durable monotonic sequence instead.
CREATE TABLE artifact_id_sequence (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    next_id   INTEGER NOT NULL
);
INSERT INTO artifact_id_sequence (singleton, next_id)
SELECT 1, COALESCE(MAX(id), 0) + 1 FROM artifacts;

DROP INDEX idx_reviews_one_draft;
CREATE UNIQUE INDEX idx_reviews_one_draft
    ON reviews(created_by, session_id, subject_kind, subject_id)
    WHERE status = 'draft';
DROP INDEX idx_reviews_subject;
CREATE INDEX idx_reviews_subject
    ON reviews(branch_id, session_id, subject_kind, subject_id, id);

ALTER TABLE reviews ADD COLUMN draft_revision INTEGER NOT NULL DEFAULT 1;

ALTER TABLE review_delivery_outbox ADD COLUMN lease_token TEXT;
ALTER TABLE review_delivery_outbox
    ADD COLUMN lease_generation INTEGER NOT NULL DEFAULT 0;
