-- Correct the staged-review contract before it ships: canonical public subject
-- keys, optimistic draft revisions, and fenced delivery leases.
ALTER TABLE reviews ADD COLUMN subject_id TEXT NOT NULL DEFAULT '';
UPDATE reviews
SET subject_id = subject_key,
    subject_key = subject_label
WHERE subject_kind = 'artifact' AND subject_id = '';

ALTER TABLE reviews ADD COLUMN draft_revision INTEGER NOT NULL DEFAULT 1;

ALTER TABLE review_delivery_outbox ADD COLUMN lease_token TEXT;
ALTER TABLE review_delivery_outbox
    ADD COLUMN lease_generation INTEGER NOT NULL DEFAULT 0;
