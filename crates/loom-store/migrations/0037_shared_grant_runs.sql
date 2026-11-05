ALTER TABLE automation_runs ADD COLUMN shared_grant_id TEXT;
CREATE UNIQUE INDEX automation_runs_shared_grant ON automation_runs(shared_grant_id)
WHERE shared_grant_id IS NOT NULL;
