ALTER TABLE processed_deliveries ADD COLUMN review_payload TEXT;
ALTER TABLE processed_deliveries ADD COLUMN review_attempt INTEGER NOT NULL DEFAULT 0;
ALTER TABLE processed_deliveries ADD COLUMN review_retry_at INTEGER NOT NULL DEFAULT 0;

CREATE INDEX processed_deliveries_pending_review
    ON processed_deliveries(review_retry_at)
    WHERE review_payload IS NOT NULL;
