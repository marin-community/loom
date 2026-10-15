ALTER TABLE conversation_inbox ADD COLUMN interrupt_at TEXT;

-- Give the existing backlog a grace period when bounded delivery is enabled.
UPDATE conversation_inbox
SET interrupt_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+60 seconds')
WHERE review_id IS NULL AND state != 'consumed';
