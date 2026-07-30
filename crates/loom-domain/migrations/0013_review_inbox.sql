-- Submitted review feedback has its own immutable conversation lane. It is
-- branch-addressable so a later live ACP session can recover an entry whose
-- preferred session became unavailable, and it is never exposed through the
-- editable sessions.pending_prompt retraction API.
CREATE TABLE review_conversation_inbox (
    delivery_key        TEXT PRIMARY KEY,
    review_id           INTEGER NOT NULL,
    branch_id           TEXT NOT NULL,
    preferred_session_id TEXT NOT NULL,
    claimed_session_id  TEXT,
    payload             TEXT NOT NULL,
    state               TEXT NOT NULL DEFAULT 'queued',
    claim_token         TEXT,
    claim_owner         TEXT,
    claimed_at          TEXT,
    consumed_at         TEXT,
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX idx_review_conversation_inbox_ready
    ON review_conversation_inbox(branch_id, state, created_at);
