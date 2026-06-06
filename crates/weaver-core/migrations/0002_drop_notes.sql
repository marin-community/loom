-- Drop the free-form `notes` table. `weaver note` is gone: progress is now
-- carried by the status-description trail set via `weaver set-status` (recorded
-- as `attention` events), so a separate note log is redundant. `IF EXISTS`
-- keeps this a no-op on databases that never had the table.
DROP TABLE IF EXISTS notes;
