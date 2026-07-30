-- Durable communication contexts. A session's default channel deliberately
-- reuses the session id as its public handle: callers already hold it, legacy
-- rows backfill deterministically, and custom channels still receive their own
-- independent ids.
CREATE TABLE channels (
    id              TEXT PRIMARY KEY,
    kind            TEXT NOT NULL,
    repo_root       TEXT NOT NULL,
    -- Logical provenance only: channels are loom-owned while branches belong
    -- to weaver-core, and custom channels must outlive a deleted session branch.
    branch_id       TEXT,
    session_id      TEXT UNIQUE REFERENCES sessions(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    topic           TEXT NOT NULL DEFAULT '',
    state           TEXT NOT NULL DEFAULT 'open',
    created_by_kind TEXT NOT NULL,
    created_by      TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    archived_at     TEXT
);

CREATE INDEX idx_channels_repo_state ON channels(repo_root, state, created_at);
CREATE INDEX idx_channels_branch ON channels(branch_id, created_at);

CREATE TABLE channel_messages (
    id              TEXT PRIMARY KEY,
    channel_id      TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    seq             INTEGER NOT NULL,
    kind            TEXT NOT NULL DEFAULT 'message',
    urgency         TEXT NOT NULL DEFAULT 'normal',
    author_kind     TEXT NOT NULL,
    author_id       TEXT NOT NULL,
    body            TEXT NOT NULL DEFAULT '',
    payload         TEXT NOT NULL DEFAULT '{}',
    reply_to        TEXT REFERENCES channel_messages(id) ON DELETE SET NULL,
    idempotency_key TEXT,
    created_at      TEXT NOT NULL,
    UNIQUE(channel_id, seq)
);

CREATE UNIQUE INDEX idx_channel_messages_idempotency
    ON channel_messages(channel_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

CREATE TABLE channel_subscriptions (
    channel_id      TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    subject_kind    TEXT NOT NULL,
    subject_id      TEXT NOT NULL,
    mode            TEXT NOT NULL DEFAULT 'observe',
    read_seq        INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    PRIMARY KEY(channel_id, subject_kind, subject_id)
);

CREATE INDEX idx_channel_subscriptions_subject
    ON channel_subscriptions(subject_kind, subject_id, updated_at);

CREATE TABLE channel_deliveries (
    message_id        TEXT NOT NULL REFERENCES channel_messages(id) ON DELETE CASCADE,
    target_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    state             TEXT NOT NULL DEFAULT 'queued',
    attempts          INTEGER NOT NULL DEFAULT 0,
    last_error        TEXT,
    updated_at        TEXT NOT NULL,
    PRIMARY KEY(message_id, target_session_id)
);

-- Existing sessions gain one default channel. Their goal is provenance in the
-- channel, not a second runtime prompt, so both the owning session and creator
-- begin read through the opening goal message.
INSERT INTO channels
    (id, kind, repo_root, branch_id, session_id, name, topic, state,
     created_by_kind, created_by, created_at, archived_at)
SELECT s.id, 'session', b.repo_root, b.id, s.id,
       CASE WHEN b.title = '' THEN b.branch ELSE b.title END,
       b.goal,
       CASE WHEN s.status = 'archived' THEN 'archived' ELSE 'open' END,
       s.creator_kind, s.creator_subject, s.created_at,
       CASE WHEN s.status = 'archived' THEN s.created_at ELSE NULL END
FROM sessions s
JOIN branches b ON b.id = s.branch_id;

INSERT INTO channel_messages
    (id, channel_id, seq, kind, urgency, author_kind, author_id, body, payload, created_at)
SELECT lower(hex(randomblob(8))), s.id, 1, 'goal', 'normal',
       s.creator_kind, s.creator_subject, b.goal, '{}', s.created_at
FROM sessions s
JOIN branches b ON b.id = s.branch_id
WHERE trim(b.goal) != '';

INSERT OR IGNORE INTO channel_subscriptions
    (channel_id, subject_kind, subject_id, mode, read_seq, created_at, updated_at)
SELECT s.id, 'session', s.id, 'deliver',
       CASE WHEN trim(b.goal) = '' THEN 0 ELSE 1 END, s.created_at, s.created_at
FROM sessions s
JOIN branches b ON b.id = s.branch_id;

INSERT OR IGNORE INTO channel_subscriptions
    (channel_id, subject_kind, subject_id, mode, read_seq, created_at, updated_at)
SELECT s.id, s.creator_kind, s.creator_subject, 'observe',
       CASE WHEN trim(b.goal) = '' THEN 0 ELSE 1 END, s.created_at, s.created_at
FROM sessions s
JOIN branches b ON b.id = s.branch_id;

INSERT OR IGNORE INTO channel_subscriptions
    (channel_id, subject_kind, subject_id, mode, read_seq, created_at, updated_at)
SELECT child.id, 'session', child.parent_session_id, 'observe',
       CASE WHEN trim(b.goal) = '' THEN 0 ELSE 1 END,
       child.created_at, child.created_at
FROM sessions child
JOIN branches b ON b.id = child.branch_id
WHERE child.parent_session_id IS NOT NULL;
