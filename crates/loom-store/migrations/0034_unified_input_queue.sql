ALTER TABLE conversation_inbox ADD COLUMN editable INTEGER NOT NULL DEFAULT 0;
ALTER TABLE conversation_inbox ADD COLUMN author TEXT;
ALTER TABLE conversation_inbox ADD COLUMN resources TEXT NOT NULL DEFAULT '[]';

INSERT INTO conversation_inbox
    (delivery_key, branch_id, preferred_session_id, payload, editable, author)
SELECT 'prompt:' || id || ':migrated', branch_id, id, pending_prompt, 1, 'manual'
FROM sessions WHERE length(trim(coalesce(pending_prompt, ''))) > 0;

ALTER TABLE sessions DROP COLUMN pending_prompt;
