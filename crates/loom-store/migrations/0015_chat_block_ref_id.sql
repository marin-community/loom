-- Promote the upstream id a chat block is addressed by out of its opaque JSON
-- payload and into an indexed column: `tool_call_id` for a `tool_call`,
-- `request_id` for a `permission_request`, empty for every other kind.
--
-- These ids answer the replay-idempotency questions ("is this tool call already
-- journaled", "was this permission already resolved"). Reading them out of the
-- payload meant loading and parsing every block of that kind for the session on
-- each question — quadratic in the length of a long-running session's journal.

ALTER TABLE chat_blocks ADD COLUMN ref_id TEXT NOT NULL DEFAULT '';

UPDATE chat_blocks
   SET ref_id = COALESCE(
           json_extract(payload, '$.tool_call_id'),
           json_extract(payload, '$.request_id'),
           ''
       )
 WHERE kind IN ('tool_call', 'permission_request');

-- `kind` ahead of `ref_id` keeps the two id namespaces apart and leaves a
-- `(session_id, kind)` prefix for the kind-scoped scans (open permissions).
CREATE INDEX IF NOT EXISTS idx_chat_blocks_ref
    ON chat_blocks(session_id, kind, ref_id);
