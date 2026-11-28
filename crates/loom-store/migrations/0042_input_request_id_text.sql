-- The agent-client-protocol engine assigns UUID strings as ACP request ids.
-- The inbox delivery record carries one to recognize the replayed response
-- after a restart, so the column must hold them verbatim.
ALTER TABLE conversation_inbox DROP COLUMN input_request_id;
ALTER TABLE conversation_inbox ADD COLUMN input_request_id TEXT;
