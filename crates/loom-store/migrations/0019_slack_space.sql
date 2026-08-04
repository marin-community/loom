-- Slack-origin sessions are conversations, so keep them in their own
-- operator-visible space instead of mixing them into the generic User inbox.
-- Adopt a manually-created Slack/Inbox when one already exists.
INSERT OR IGNORE INTO session_spaces
    (id, name, rank, system_key, created_at, updated_at)
SELECT
    'space-slack',
    'Slack',
    COALESCE(MAX(rank) + 1, 0),
    'slack',
    strftime('%Y-%m-%dT%H:%M:%fZ','now'),
    strftime('%Y-%m-%dT%H:%M:%fZ','now')
FROM session_spaces;

UPDATE session_spaces
SET system_key = 'slack',
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE name = 'Slack' COLLATE NOCASE
  AND system_key IS NULL;

INSERT OR IGNORE INTO session_groups
    (id, space_id, name, rank, system_key, created_at, updated_at)
SELECT
    'group-slack-inbox',
    id,
    'Inbox',
    0,
    'inbox',
    strftime('%Y-%m-%dT%H:%M:%fZ','now'),
    strftime('%Y-%m-%dT%H:%M:%fZ','now')
FROM session_spaces
WHERE system_key = 'slack';

UPDATE session_groups
SET system_key = 'inbox',
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE space_id = (
        SELECT id FROM session_spaces WHERE system_key = 'slack'
    )
  AND name = 'Inbox' COLLATE NOCASE
  AND system_key IS NULL;

-- Preserve an operator's explicit origin default if they configured one before
-- this system space existed.
INSERT OR IGNORE INTO session_placement_defaults
    (selector_kind, selector_value, group_id)
SELECT
    'origin',
    'slack',
    session_groups.id
FROM session_groups
JOIN session_spaces ON session_spaces.id = session_groups.space_id
WHERE session_spaces.system_key = 'slack'
  AND session_groups.system_key = 'inbox';

-- Move only legacy Slack sessions still sitting in the old fallback inbox.
-- Manual placements remain untouched.
UPDATE session_placements
SET group_id = (
        SELECT session_groups.id
        FROM session_groups
        JOIN session_spaces ON session_spaces.id = session_groups.space_id
        WHERE session_spaces.system_key = 'slack'
          AND session_groups.system_key = 'inbox'
    ),
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE group_id = 'group-user-inbox'
  AND session_id IN (
      SELECT id FROM sessions WHERE origin = 'slack'
  );
