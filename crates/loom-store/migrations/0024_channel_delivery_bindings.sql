-- Generalize runtime-only delivery receipts into channel binding receipts.
-- Existing rows retain their session target and gain a stable namespaced
-- binding id. Provider coordinates remain outside this table; only the
-- provider-returned external message id is stored.
ALTER TABLE channel_deliveries RENAME TO channel_deliveries_v23;

CREATE TABLE channel_deliveries (
    message_id        TEXT NOT NULL REFERENCES channel_messages(id) ON DELETE CASCADE,
    binding_id        TEXT NOT NULL,
    binding_kind      TEXT NOT NULL,
    target_session_id TEXT REFERENCES sessions(id) ON DELETE CASCADE,
    state             TEXT NOT NULL DEFAULT 'queued',
    attempts          INTEGER NOT NULL DEFAULT 0,
    last_error        TEXT,
    external_id       TEXT,
    updated_at        TEXT NOT NULL,
    PRIMARY KEY(message_id, binding_id)
);

INSERT INTO channel_deliveries
    (message_id, binding_id, binding_kind, target_session_id, state, attempts,
     last_error, updated_at)
SELECT message_id, 'session:' || target_session_id, 'session', target_session_id,
       state, attempts, last_error, updated_at
FROM channel_deliveries_v23;

DROP TABLE channel_deliveries_v23;

CREATE INDEX idx_channel_deliveries_target_session
    ON channel_deliveries(target_session_id, updated_at);
