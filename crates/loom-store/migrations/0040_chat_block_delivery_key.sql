-- Restart overlaps the old and new server. Derive the key in SQLite so writes
-- from either version have the same indexed delivery identity.
ALTER TABLE chat_blocks ADD COLUMN delivery_key TEXT GENERATED ALWAYS AS (
    CASE WHEN kind = 'user_message' AND json_valid(payload) THEN
        CASE WHEN json_type(payload, '$.delivery_key') = 'text'
             THEN NULLIF(json_extract(payload, '$.delivery_key'), '') END
    END
) VIRTUAL;

CREATE INDEX idx_chat_blocks_delivery ON chat_blocks(session_id, delivery_key, turn)
    WHERE delivery_key IS NOT NULL;
