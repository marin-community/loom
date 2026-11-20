-- Seek to the next session in the index instead of walking every block entry.
-- Only user-message payloads need to be read from the journal itself.
WITH RECURSIVE journal(session_id) AS (
    SELECT MIN(session_id) FROM chat_blocks
    UNION ALL
    SELECT (SELECT MIN(session_id) FROM chat_blocks WHERE session_id > journal.session_id)
      FROM journal WHERE session_id IS NOT NULL
), keyed_blocks AS MATERIALIZED (
    SELECT block.id,
           CASE WHEN json_valid(block.payload) THEN
               CASE WHEN json_type(block.payload, '$.delivery_key') = 'text'
                    THEN NULLIF(json_extract(block.payload, '$.delivery_key'), '') END
           END AS delivery_key
      FROM journal
      CROSS JOIN chat_blocks AS block INDEXED BY idx_chat_blocks_ref
     WHERE block.session_id = journal.session_id
       AND block.kind = 'user_message'
       AND block.ref_id = ''
)
UPDATE chat_blocks
   SET ref_id = keyed_blocks.delivery_key
  FROM keyed_blocks
 WHERE chat_blocks.id = keyed_blocks.id AND keyed_blocks.delivery_key IS NOT NULL;

-- The previous server may still write to the journal while it drains.
CREATE TRIGGER chat_blocks_user_delivery_ref
AFTER INSERT ON chat_blocks
WHEN NEW.kind = 'user_message' AND NEW.ref_id = ''
 AND CASE WHEN json_valid(NEW.payload) THEN
          json_type(NEW.payload, '$.delivery_key') = 'text'
          AND json_extract(NEW.payload, '$.delivery_key') <> ''
     END
BEGIN
    UPDATE chat_blocks SET ref_id = json_extract(NEW.payload, '$.delivery_key') WHERE id = NEW.id;
END;
