-- Shared, durable fleet organization. Machine provenance (origin/class/lineage)
-- remains on sessions; these tables own only the operator-controlled layout.
CREATE TABLE session_layout_state (
    id       INTEGER PRIMARY KEY CHECK (id = 1),
    revision INTEGER NOT NULL
);

INSERT INTO session_layout_state (id, revision) VALUES (1, 1);

CREATE TABLE session_spaces (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL COLLATE NOCASE UNIQUE,
    rank       INTEGER NOT NULL,
    system_key TEXT UNIQUE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE session_groups (
    id         TEXT PRIMARY KEY,
    space_id   TEXT NOT NULL REFERENCES session_spaces(id) ON DELETE CASCADE,
    name       TEXT NOT NULL COLLATE NOCASE,
    rank       INTEGER NOT NULL,
    system_key TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(space_id, name),
    UNIQUE(space_id, system_key)
);

CREATE TABLE session_placements (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    group_id   TEXT NOT NULL REFERENCES session_groups(id),
    rank       INTEGER NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE user_session_group_state (
    user_id    TEXT NOT NULL REFERENCES users(username) ON DELETE CASCADE,
    group_id   TEXT NOT NULL REFERENCES session_groups(id) ON DELETE CASCADE,
    collapsed  INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, group_id)
);

CREATE TABLE session_placement_defaults (
    selector_kind  TEXT NOT NULL,
    selector_value TEXT NOT NULL,
    group_id       TEXT NOT NULL REFERENCES session_groups(id),
    PRIMARY KEY (selector_kind, selector_value)
);

INSERT INTO session_spaces (id, name, rank, system_key, created_at, updated_at)
VALUES
    ('space-user', 'User', 0, 'user', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    ('space-github', 'GitHub', 1, 'github', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    ('space-ops', 'Ops', 2, 'ops', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));

INSERT INTO session_groups (id, space_id, name, rank, system_key, created_at, updated_at)
VALUES
    ('group-user-inbox', 'space-user', 'Inbox', 0, 'inbox', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    ('group-github-inbox', 'space-github', 'Inbox', 0, 'inbox', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    ('group-ops-inbox', 'space-ops', 'Inbox', 0, 'inbox', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));

-- Resolve delegated sessions through their closest placed ancestor. Orphans
-- deterministically fall back to User; no browser-derived idle shelf is
-- migrated.
WITH RECURSIVE effective_space(session_id, space_id) AS (
    SELECT
        s.id,
        CASE s.origin
            WHEN 'github' THEN 'space-github'
            WHEN 'watch' THEN 'space-ops'
            WHEN 'actions' THEN 'space-ops'
            WHEN 'ops' THEN 'space-ops'
            WHEN 'grafana' THEN 'space-ops'
            WHEN 'automation' THEN 'space-ops'
            ELSE 'space-user'
        END
    FROM sessions s
    WHERE s.managed_by IS NULL
      AND (s.origin != 'agent'
       OR s.parent_session_id IS NULL
       OR NOT EXISTS (SELECT 1 FROM sessions parent WHERE parent.id = s.parent_session_id))
    UNION ALL
    SELECT child.id, parent.space_id
    FROM sessions child
    JOIN effective_space parent ON parent.session_id = child.parent_session_id
    WHERE child.managed_by IS NULL
      AND child.origin = 'agent' AND child.id != child.parent_session_id
)
INSERT INTO session_groups (id, space_id, name, rank, system_key, created_at, updated_at)
SELECT
    CASE space_id
        WHEN 'space-github' THEN 'group-github-later'
        WHEN 'space-ops' THEN 'group-ops-later'
        ELSE 'group-user-later'
    END,
    space_id,
    'Later',
    1,
    'later',
    strftime('%Y-%m-%dT%H:%M:%fZ','now'),
    strftime('%Y-%m-%dT%H:%M:%fZ','now')
FROM effective_space
JOIN sessions ON sessions.id = effective_space.session_id
WHERE sessions.park = 'parked'
GROUP BY space_id;

WITH RECURSIVE planned(
    session_id, group_id, created_at, last_activity_at, park, sort_order
) AS (
    SELECT
        s.id,
        CASE
            WHEN s.park = 'parked' THEN
                CASE s.origin
                    WHEN 'github' THEN 'group-github-later'
                    WHEN 'watch' THEN 'group-ops-later'
                    WHEN 'actions' THEN 'group-ops-later'
                    WHEN 'ops' THEN 'group-ops-later'
                    WHEN 'grafana' THEN 'group-ops-later'
                    WHEN 'automation' THEN 'group-ops-later'
                    ELSE 'group-user-later'
                END
            ELSE
                CASE s.origin
                    WHEN 'github' THEN 'group-github-inbox'
                    WHEN 'watch' THEN 'group-ops-inbox'
                    WHEN 'actions' THEN 'group-ops-inbox'
                    WHEN 'ops' THEN 'group-ops-inbox'
                    WHEN 'grafana' THEN 'group-ops-inbox'
                    WHEN 'automation' THEN 'group-ops-inbox'
                    ELSE 'group-user-inbox'
                END
        END,
        s.created_at,
        s.last_activity_at,
        s.park,
        s.sort_order
    FROM sessions s
    WHERE s.managed_by IS NULL
      AND (s.origin != 'agent'
       OR s.parent_session_id IS NULL
       OR NOT EXISTS (SELECT 1 FROM sessions parent WHERE parent.id = s.parent_session_id))
    UNION ALL
    SELECT
        child.id,
        CASE
            WHEN child.park = 'parked' THEN (
                SELECT later.id
                FROM session_groups inherited
                JOIN session_groups later
                  ON later.space_id = inherited.space_id AND later.system_key = 'later'
                WHERE inherited.id = parent.group_id
            )
            ELSE parent.group_id
        END,
        child.created_at,
        child.last_activity_at,
        child.park,
        child.sort_order
    FROM sessions child
    JOIN planned parent ON parent.session_id = child.parent_session_id
    WHERE child.managed_by IS NULL
      AND child.origin = 'agent' AND child.id != child.parent_session_id
),
ranked AS (
    SELECT
        session_id,
        group_id,
        ROW_NUMBER() OVER (
            PARTITION BY group_id
            ORDER BY
                CASE
                    -- The old Later shelf ignored manual drag keys and showed
                    -- the most recently active parked work first.
                    WHEN park = 'parked' THEN
                        -CAST((
                            julianday(COALESCE(NULLIF(last_activity_at, ''), created_at))
                            - 2440587.5
                        ) * 86400000 AS INTEGER)
                    -- Old manual keys shared the browser's negative Unix-ms
                    -- axis with untouched rows, which were newest-first.
                    ELSE COALESCE(
                        sort_order,
                        -CAST((
                            julianday(created_at) - 2440587.5
                        ) * 86400000 AS INTEGER)
                    )
                END,
                session_id
        ) - 1 AS rank
    FROM planned
    WHERE group_id IS NOT NULL
)
INSERT INTO session_placements (session_id, group_id, rank, updated_at)
SELECT session_id, group_id, rank, strftime('%Y-%m-%dT%H:%M:%fZ','now')
FROM ranked;

-- A malformed ancestry cycle is not allowed to defeat canonical placement.
INSERT OR IGNORE INTO session_groups
    (id, space_id, name, rank, system_key, created_at, updated_at)
SELECT
    'group-user-later',
    'space-user',
    'Later',
    1,
    'later',
    strftime('%Y-%m-%dT%H:%M:%fZ','now'),
    strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE EXISTS (
    SELECT 1
    FROM sessions s
    WHERE s.managed_by IS NULL
      AND s.park = 'parked'
      AND NOT EXISTS (
          SELECT 1 FROM session_placements p WHERE p.session_id = s.id
      )
);

WITH missing AS (
    SELECT
        s.id AS session_id,
        CASE WHEN s.park = 'parked' THEN 'group-user-later' ELSE 'group-user-inbox' END
            AS group_id,
        s.created_at,
        s.last_activity_at,
        s.park,
        s.sort_order
    FROM sessions s
    WHERE s.managed_by IS NULL
      AND NOT EXISTS (
        SELECT 1 FROM session_placements p WHERE p.session_id = s.id
    )
),
ranked_missing AS (
    SELECT
        session_id,
        group_id,
        COALESCE((
            SELECT MAX(existing.rank) + 1
            FROM session_placements existing
            WHERE existing.group_id = missing.group_id
        ), 0)
        + ROW_NUMBER() OVER (
            PARTITION BY group_id
            ORDER BY
                CASE
                    WHEN park = 'parked' THEN
                        -CAST((
                            julianday(COALESCE(NULLIF(last_activity_at, ''), created_at))
                            - 2440587.5
                        ) * 86400000 AS INTEGER)
                    ELSE COALESCE(
                        sort_order,
                        -CAST((
                            julianday(created_at) - 2440587.5
                        ) * 86400000 AS INTEGER)
                    )
                END,
                session_id
        ) - 1 AS rank
    FROM missing
)
INSERT INTO session_placements (session_id, group_id, rank, updated_at)
SELECT session_id, group_id, rank, strftime('%Y-%m-%dT%H:%M:%fZ','now')
FROM ranked_missing;

INSERT INTO session_placement_defaults (selector_kind, selector_value, group_id)
VALUES
    ('origin', '*', 'group-user-inbox'),
    ('origin', 'user', 'group-user-inbox'),
    ('origin', 'github', 'group-github-inbox'),
    ('origin', 'watch', 'group-ops-inbox'),
    ('origin', 'actions', 'group-ops-inbox'),
    ('origin', 'ops', 'group-ops-inbox'),
    ('origin', 'grafana', 'group-ops-inbox'),
    ('origin', 'automation', 'group-ops-inbox');
