-- Rename the overlooker subsystem to its real name: watches.
--
-- `overlookers` → `watches`, `overlooker_runs` → `watch_runs` (with its
-- `overlooker_id` join column becoming `watch_id`), and every stored
-- `overlooker.*` settings key becomes the matching `watch.*` key. Pure rename:
-- no shape change, no data loss.
ALTER TABLE overlookers RENAME TO watches;
ALTER TABLE overlooker_runs RENAME TO watch_runs;
ALTER TABLE watch_runs RENAME COLUMN overlooker_id TO watch_id;
DROP INDEX IF EXISTS idx_overlooker_runs;
CREATE INDEX IF NOT EXISTS idx_watch_runs ON watch_runs(watch_id, id);
UPDATE settings SET key = 'watch.' || substr(key, length('overlooker.') + 1)
 WHERE key LIKE 'overlooker.%';
