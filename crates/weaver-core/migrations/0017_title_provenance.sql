ALTER TABLE branches
ADD COLUMN title_provenance TEXT NOT NULL DEFAULT 'user'
CHECK (title_provenance IN ('derived', 'generated', 'user', 'issue'));

-- Existing titles are human-owned unless the stored goal proves the exact
-- deterministic derivation without having to reproduce Unicode truncation in
-- SQLite. This deliberately conservative subset is safe to auto-replace.
UPDATE branches
SET title_provenance = 'derived'
WHERE instr(goal, char(10)) = 0
  AND instr(goal, char(13)) = 0
  AND length(trim(goal)) BETWEEN 1 AND 72
  AND title = trim(goal);
