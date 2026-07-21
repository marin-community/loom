-- Loom-owned schema at the point its independent migration stream was
-- introduced. Existing unversioned databases are brought to this shape by the
-- adoption code in db.rs and then stamped at version 1; fresh databases run
-- this file normally.

CREATE TABLE IF NOT EXISTS sessions (
    id                 TEXT PRIMARY KEY,
    branch_id          TEXT NOT NULL REFERENCES branches(id) ON DELETE CASCADE,
    work_dir           TEXT NOT NULL,
    term_session       TEXT NOT NULL,
    agent_kind         TEXT NOT NULL DEFAULT 'claude',
    model              TEXT NOT NULL DEFAULT '',
    effort             TEXT NOT NULL DEFAULT '',
    status             TEXT NOT NULL,
    github_repo        TEXT,
    last_activity_at   TEXT,
    parent_branch_id   TEXT,
    managed_by         TEXT,
    created_by         TEXT,
    park               TEXT,
    sort_order         REAL,
    protocol           TEXT NOT NULL DEFAULT 'terminal',
    acp_session_id     TEXT,
    acp_ack_seq        INTEGER NOT NULL DEFAULT 0,
    acp_inflight       TEXT,
    current_mode       TEXT,
    pending_prompt     TEXT NOT NULL DEFAULT '',
    origin             TEXT NOT NULL DEFAULT 'user',
    class              TEXT NOT NULL DEFAULT 'interactive',
    turn_count         INTEGER NOT NULL DEFAULT 0,
    tracking_issue_id  INTEGER,
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_active_branch
    ON sessions(branch_id) WHERE status NOT IN ('done', 'error', 'archived');

CREATE TABLE IF NOT EXISTS recent_repos (
    repo_root    TEXT PRIMARY KEY,
    last_used_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE IF NOT EXISTS repos (
    slug       TEXT PRIMARY KEY,
    remote_url TEXT NOT NULL,
    path       TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE IF NOT EXISTS processed_deliveries (
    delivery_id TEXT PRIMARY KEY,
    received_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE IF NOT EXISTS branch_github (
    branch_id        TEXT PRIMARY KEY REFERENCES branches(id) ON DELETE CASCADE,
    pr_number        INTEGER,
    pr_url           TEXT,
    pr_state         TEXT,
    pr_title         TEXT,
    is_draft         INTEGER NOT NULL DEFAULT 0,
    review_decision  TEXT,
    checks           TEXT,
    mergeable        TEXT,
    merged_at        TEXT,
    fetched_at       TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS branch_github_mapping (
    branch_id  TEXT PRIMARY KEY REFERENCES branches(id) ON DELETE CASCADE,
    pr_number  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS users (
    username       TEXT PRIMARY KEY,
    github_login   TEXT UNIQUE,
    password_hash  TEXT,
    github_user_id INTEGER,
    display_name   TEXT,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE IF NOT EXISTS api_tokens (
    id           TEXT PRIMARY KEY,
    username     TEXT NOT NULL REFERENCES users(username) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    token_hash   TEXT NOT NULL UNIQUE,
    prefix       TEXT NOT NULL,
    kind         TEXT NOT NULL DEFAULT 'pat',
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    last_used_at TEXT,
    expires_at   TEXT
);

CREATE TABLE IF NOT EXISTS agent_env (
    name       TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE IF NOT EXISTS repo_env (
    repo_root  TEXT NOT NULL,
    name       TEXT NOT NULL,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (repo_root, name)
);

CREATE TABLE IF NOT EXISTS auth_sessions (
    token_hash TEXT PRIMARY KEY,
    username   TEXT NOT NULL REFERENCES users(username) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    expires_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS user_github_tokens (
    username   TEXT PRIMARY KEY REFERENCES users(username) ON DELETE CASCADE,
    token      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE IF NOT EXISTS custom_agents (
    name           TEXT PRIMARY KEY,
    label          TEXT NOT NULL,
    setup          TEXT NOT NULL DEFAULT '',
    launch         TEXT NOT NULL DEFAULT '',
    resume         TEXT NOT NULL DEFAULT '',
    reports_status INTEGER NOT NULL DEFAULT 0,
    protocol       TEXT NOT NULL DEFAULT 'terminal',
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE IF NOT EXISTS chat_blocks (
    id         INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn       INTEGER NOT NULL,
    seq        INTEGER NOT NULL,
    kind       TEXT NOT NULL,
    payload    TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(session_id, turn, seq)
);

CREATE INDEX IF NOT EXISTS idx_chat_blocks_session
    ON chat_blocks(session_id, turn, seq);
