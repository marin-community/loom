-- The agent-client-protocol engine assigns UUID strings as ACP request ids.
-- The inbox delivery record carries one to recognize the replayed response
-- after a restart, so the column must hold them verbatim. Convert rather than
-- recreate: a delivery in flight across the upgrade keeps its legacy numeric
-- id, which still matches the replayed response (ids render bare on the wire).
ALTER TABLE conversation_inbox ADD COLUMN input_request_id_text TEXT;
UPDATE conversation_inbox SET input_request_id_text = CAST(input_request_id AS TEXT);
ALTER TABLE conversation_inbox DROP COLUMN input_request_id;
ALTER TABLE conversation_inbox RENAME COLUMN input_request_id_text TO input_request_id;
