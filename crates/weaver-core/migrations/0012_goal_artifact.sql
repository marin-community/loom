-- Promote each branch's existing goal into a branch-scoped `goal` artifact
-- (rev 1, authored by the user who wrote it). The branches.goal column stays as
-- a denormalized cache; from here it is kept in sync from the artifact.
INSERT INTO artifacts (repo_root, branch_id, name, kind, title)
SELECT b.repo_root, b.id, 'goal', 'markdown', 'Goal'
FROM branches b
WHERE b.goal <> ''
  AND NOT EXISTS (
    SELECT 1 FROM artifacts a WHERE a.branch_id = b.id AND a.name = 'goal'
  );

INSERT INTO artifact_versions (artifact_id, rev, author, content)
SELECT a.id, 1, 'user', b.goal
FROM artifacts a
JOIN branches b ON b.id = a.branch_id
WHERE a.name = 'goal'
  AND NOT EXISTS (
    SELECT 1 FROM artifact_versions v WHERE v.artifact_id = a.id
  );
