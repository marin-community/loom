-- The watch execution log: capture each round's script output so a user can
-- click a watch and see exactly what every run printed, returned, and how long
-- it took — not just its one-line summary.
--
-- `trigger_event` is the normalized event that woke the round (`cron` /
-- `manual` / e.g. `pr.merged`), distinct from the free-form `trigger_reason`.
-- `stdout`/`stderr` are tails of the script's captured streams; `exit_code` is
-- the interpreter's exit status (NULL when it never spawned or timed out);
-- `duration_ms` is the wall-clock the program ran.
ALTER TABLE overlooker_runs ADD COLUMN trigger_event TEXT NOT NULL DEFAULT '';
ALTER TABLE overlooker_runs ADD COLUMN stdout        TEXT NOT NULL DEFAULT '';
ALTER TABLE overlooker_runs ADD COLUMN stderr        TEXT NOT NULL DEFAULT '';
ALTER TABLE overlooker_runs ADD COLUMN exit_code     INTEGER;
ALTER TABLE overlooker_runs ADD COLUMN duration_ms   INTEGER;
