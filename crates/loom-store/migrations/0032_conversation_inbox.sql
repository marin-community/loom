-- Session messages share the protected inbox, but remain addressed to one
-- session. Only submitted reviews may follow a replacement on the branch.
ALTER TABLE review_conversation_inbox RENAME TO conversation_inbox_old;
CREATE TABLE conversation_inbox (
    delivery_key TEXT PRIMARY KEY,
    review_id INTEGER,
    branch_id TEXT NOT NULL,
    preferred_session_id TEXT NOT NULL,
    claimed_session_id TEXT,
    payload TEXT NOT NULL,
    source_session_id TEXT,
    source_channel_id TEXT,
    source_message_id TEXT,
    state TEXT NOT NULL DEFAULT 'queued',
    claim_token TEXT,
    claim_owner TEXT,
    claimed_at TEXT,
    consumed_at TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
INSERT INTO conversation_inbox
    (delivery_key, review_id, branch_id, preferred_session_id, claimed_session_id,
     payload, state, claim_token, claim_owner, claimed_at, consumed_at, created_at)
SELECT delivery_key, review_id, branch_id, preferred_session_id, claimed_session_id,
       payload, state, claim_token, claim_owner, claimed_at, consumed_at, created_at
FROM conversation_inbox_old;
DROP TABLE conversation_inbox_old;
CREATE INDEX idx_conversation_inbox_ready
    ON conversation_inbox(branch_id, state, created_at);
