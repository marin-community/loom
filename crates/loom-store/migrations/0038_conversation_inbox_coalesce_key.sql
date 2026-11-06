-- A queued input that a newer input with the same key replaces in place, such
-- as the message that a person edited a session's doc.
ALTER TABLE conversation_inbox ADD COLUMN coalesce_key TEXT;
