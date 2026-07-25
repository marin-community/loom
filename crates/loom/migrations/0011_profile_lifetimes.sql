ALTER TABLE profiles
ADD COLUMN lifetime INTEGER NOT NULL DEFAULT 1;

ALTER TABLE sessions
ADD COLUMN profile_lifetime INTEGER NOT NULL DEFAULT 0;

ALTER TABLE sessions
ADD COLUMN policy_strict INTEGER NOT NULL DEFAULT 0;

ALTER TABLE sessions
ADD COLUMN mutation_revision INTEGER NOT NULL DEFAULT 1;

-- A pre-lifetime session is safe to associate with the current profile row
-- only when its stamped revision exactly proves that row has not crossed a
-- recreate. A retired row one revision ahead is ambiguous in released v9:
-- retire -> revive -> retire can leave a replacement tombstone at that shape.
-- Fail closed rather than blessing replacement credentials as the old lifetime.
UPDATE sessions
SET profile_lifetime = 1
WHERE EXISTS (
    SELECT 1
    FROM profiles
    WHERE profiles.name = sessions.profile
      AND profiles.revision = sessions.profile_revision
);

-- Phase-2 sessions already carry strictness in their immutable snapshot. Rows
-- upgraded from the earlier schema have an empty snapshot, so preserve the
-- best available launch-time contract from the still-provably-related profile.
UPDATE sessions
SET policy_strict = CASE
    WHEN launch_snapshot <> '' AND json_valid(launch_snapshot)
    THEN COALESCE(json_extract(launch_snapshot, '$.policy.strict'), 0)
    WHEN profile_lifetime <> 0
    THEN COALESCE((
        SELECT strict
        FROM profiles
        WHERE profiles.name = sessions.profile
    ), 0)
    ELSE 0
END;

-- Keep previously stamped phase-2 snapshots readable by the expanded wire DTO
-- and expose the same lifetime identity through both session observability
-- surfaces.
UPDATE sessions
SET launch_snapshot = json_set(
    launch_snapshot,
    '$.profile_lifetime',
    profile_lifetime
)
WHERE launch_snapshot <> ''
  AND json_valid(launch_snapshot);
