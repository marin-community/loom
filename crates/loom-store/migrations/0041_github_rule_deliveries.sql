ALTER TABLE processed_deliveries ADD COLUMN rule_payload TEXT;
ALTER TABLE processed_deliveries ADD COLUMN rule_attempt INTEGER NOT NULL DEFAULT 0;
ALTER TABLE processed_deliveries ADD COLUMN rule_retry_at INTEGER NOT NULL DEFAULT 0;

CREATE INDEX processed_deliveries_pending_rule
    ON processed_deliveries(rule_retry_at)
    WHERE rule_payload IS NOT NULL;
