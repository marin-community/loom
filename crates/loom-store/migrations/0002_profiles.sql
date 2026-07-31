CREATE TABLE profiles (
    name                  TEXT PRIMARY KEY,
    description           TEXT NOT NULL DEFAULT '',
    agent_kind            TEXT NOT NULL,
    model                 TEXT NOT NULL DEFAULT '',
    effort                TEXT NOT NULL DEFAULT '',
    protocol              TEXT NOT NULL,
    mode                  TEXT NOT NULL,
    class                 TEXT NOT NULL,
    strict                INTEGER NOT NULL DEFAULT 0,
    env_clear             INTEGER NOT NULL DEFAULT 0,
    ambient_allowlist     TEXT NOT NULL DEFAULT '[]',
    idle_archive_secs     INTEGER,
    max_concurrent        INTEGER NOT NULL DEFAULT 0,
    turn_budget           INTEGER,
    revision              INTEGER NOT NULL DEFAULT 1,
    created_at            TEXT NOT NULL,
    updated_at            TEXT NOT NULL
);

INSERT INTO profiles (
    name, description, agent_kind, model, effort, protocol, mode, class,
    strict, env_clear, ambient_allowlist, idle_archive_secs, max_concurrent,
    turn_budget, created_at, updated_at
)
VALUES (
    'default', 'Default interactive session profile',
    COALESCE((SELECT value FROM settings WHERE key = 'agent.default'), 'claude'),
    COALESCE((SELECT value FROM settings WHERE key = 'agent.model'), ''),
    COALESCE((SELECT value FROM settings WHERE key = 'agent.effort'), ''),
    'acp',
    COALESCE((SELECT value FROM settings WHERE key = 'agent.mode'), 'auto'),
    'interactive', 0, 0, '[]', NULL, 0, NULL,
    strftime('%Y-%m-%dT%H:%M:%fZ','now'),
    strftime('%Y-%m-%dT%H:%M:%fZ','now')
);

CREATE TABLE profile_env (
    profile_name TEXT NOT NULL REFERENCES profiles(name) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    value        TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    PRIMARY KEY (profile_name, name)
);

INSERT INTO profile_env (profile_name, name, value, updated_at)
SELECT 'default', name, value, updated_at FROM agent_env;

DROP TABLE agent_env;

ALTER TABLE sessions ADD COLUMN profile TEXT NOT NULL DEFAULT 'default';
ALTER TABLE sessions ADD COLUMN launch_mode TEXT NOT NULL DEFAULT 'auto';
ALTER TABLE sessions ADD COLUMN profile_revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE sessions ADD COLUMN policy_env_clear INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN policy_ambient_allowlist TEXT NOT NULL DEFAULT '[]';
ALTER TABLE sessions ADD COLUMN policy_idle_archive_secs INTEGER;
ALTER TABLE sessions ADD COLUMN policy_turn_budget INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN creator_kind TEXT NOT NULL DEFAULT 'system';
ALTER TABLE sessions ADD COLUMN creator_subject TEXT NOT NULL DEFAULT 'system';
ALTER TABLE sessions ADD COLUMN parent_session_id TEXT;
ALTER TABLE sessions ADD COLUMN automation_run_id TEXT;

UPDATE sessions
SET launch_mode = COALESCE(
        NULLIF(current_mode, ''),
        (SELECT value FROM settings WHERE key = 'agent.mode'),
        'auto'
    ),
    policy_idle_archive_secs = CASE
        WHEN class = 'automation' THEN COALESCE(
            CAST((SELECT value FROM settings WHERE key = 'automation.idle_archive_secs') AS INTEGER),
            28800
        )
        ELSE 0
    END,
    policy_turn_budget = CASE
        WHEN class = 'automation' THEN COALESCE(
            CAST((SELECT value FROM settings WHERE key = 'automation.turn_cap') AS INTEGER),
            100
        )
        ELSE 0
    END,
    creator_kind = CASE
        WHEN origin = 'agent' THEN 'session'
        WHEN origin IN ('actions', 'ops') THEN 'automation'
        WHEN created_by IS NOT NULL THEN 'user'
        ELSE 'system'
    END,
    creator_subject = COALESCE(created_by, origin, 'system');

UPDATE sessions AS child
SET parent_session_id = (
    SELECT parent.id
    FROM sessions AS parent
    WHERE parent.branch_id = child.parent_branch_id
      AND parent.created_at <= child.created_at
    ORDER BY parent.created_at DESC
    LIMIT 1
)
WHERE child.parent_branch_id IS NOT NULL;

CREATE INDEX idx_sessions_parent_session ON sessions(parent_session_id);
CREATE INDEX idx_sessions_creator ON sessions(creator_kind, creator_subject);
