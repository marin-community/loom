-- Later is a destination, not a queue operators need to scan inside every
-- provenance space. Consolidate the legacy per-space Later groups into one
-- top-level space and keep the compatibility `park` projection through the
-- destination group's system key.
UPDATE session_spaces
SET rank = rank + 1,
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now');

INSERT OR IGNORE INTO session_spaces
    (id, name, rank, system_key, created_at, updated_at)
VALUES
    ('space-later', 'Later', 0, 'later',
     strftime('%Y-%m-%dT%H:%M:%fZ','now'),
     strftime('%Y-%m-%dT%H:%M:%fZ','now'));

-- Adopt an operator-created Later space when one predates this migration.
UPDATE session_spaces
SET rank = 0,
    system_key = 'later',
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE name = 'Later' COLLATE NOCASE
  AND (system_key IS NULL OR system_key = 'later');

INSERT OR IGNORE INTO session_groups
    (id, space_id, name, rank, system_key, created_at, updated_at)
SELECT
    'group-later-inbox', id, 'Inbox', 0, 'later',
    strftime('%Y-%m-%dT%H:%M:%fZ','now'),
    strftime('%Y-%m-%dT%H:%M:%fZ','now')
FROM session_spaces
WHERE system_key = 'later';

-- Likewise, keep an existing Inbox and make it the compatibility Later group.
UPDATE session_groups
SET rank = 0,
    system_key = 'later',
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE space_id = (SELECT id FROM session_spaces WHERE system_key = 'later')
  AND name = 'Inbox' COLLATE NOCASE
  AND (system_key IS NULL OR system_key = 'later');

-- Merge each old Later queue in stable space/group/session order. Placements
-- already in an adopted Later/Inbox participate in the same ordering.
WITH ordered AS (
    SELECT
        placement.session_id,
        ROW_NUMBER() OVER (
            ORDER BY space.rank, group_row.rank, placement.rank, placement.session_id
        ) - 1 AS new_rank
    FROM session_placements placement
    JOIN session_groups group_row ON group_row.id = placement.group_id
    JOIN session_spaces space ON space.id = group_row.space_id
    WHERE group_row.system_key = 'later'
)
UPDATE session_placements
SET group_id = (
        SELECT id
        FROM session_groups
        WHERE space_id = (SELECT id FROM session_spaces WHERE system_key = 'later')
          AND system_key = 'later'
    ),
    rank = (SELECT new_rank FROM ordered WHERE ordered.session_id = session_placements.session_id),
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE session_id IN (SELECT session_id FROM ordered);

UPDATE session_placement_defaults
SET group_id = (
    SELECT id
    FROM session_groups
    WHERE space_id = (SELECT id FROM session_spaces WHERE system_key = 'later')
      AND system_key = 'later'
)
WHERE group_id IN (
    SELECT id
    FROM session_groups
    WHERE system_key = 'later'
      AND space_id != (SELECT id FROM session_spaces WHERE system_key = 'later')
);

DELETE FROM session_groups
WHERE system_key = 'later'
  AND space_id != (SELECT id FROM session_spaces WHERE system_key = 'later');

-- Existing interactive sessions whose profile inherited the old disabled
-- default should inherit the new ten-day default too. An explicit profile 0
-- remains an opt-out, as does the per-session `auto-archive: disabled` tag.
UPDATE sessions
SET policy_idle_archive_secs = 864000
WHERE class != 'automation'
  AND COALESCE(policy_idle_archive_secs, 0) = 0
  AND (
      SELECT idle_archive_secs
      FROM profiles
      WHERE profiles.name = sessions.profile
  ) IS NULL;

UPDATE session_layout_state SET revision = revision + 1 WHERE id = 1;
