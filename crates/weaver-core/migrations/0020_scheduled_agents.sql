ALTER TABLE watches ADD COLUMN agent_spec TEXT;
ALTER TABLE watches ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE watches ADD COLUMN deployment_managed INTEGER NOT NULL DEFAULT 0;
ALTER TABLE watches ADD COLUMN paused INTEGER NOT NULL DEFAULT 0;
ALTER TABLE watches ADD COLUMN misfire_policy TEXT NOT NULL DEFAULT 'coalesce';
ALTER TABLE watches ADD COLUMN late_grace_secs INTEGER NOT NULL DEFAULT 600;
ALTER TABLE watches ADD COLUMN run_timeout_secs INTEGER NOT NULL DEFAULT 300;
ALTER TABLE watches ADD COLUMN state_version INTEGER NOT NULL DEFAULT 0;

-- A script cannot be translated into an agent prompt without operator intent.
INSERT INTO watch_runs (watch_id, trigger_reason, trigger_event, started_at, finished_at, outcome, summary)
SELECT id, 'migration', 'migration', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
       strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), 'skipped',
       'Scheduled scripts are disabled. Configure an agent prompt and profile before enabling.'
FROM watches WHERE json_extract(trigger_spec, '$.cron') IS NOT NULL
                OR json_extract(trigger_spec, '$.every') IS NOT NULL;
UPDATE watches SET enabled = 0, next_run_at = NULL
WHERE json_extract(trigger_spec, '$.cron') IS NOT NULL
   OR json_extract(trigger_spec, '$.every') IS NOT NULL;

CREATE TABLE watch_occurrences (
    id TEXT PRIMARY KEY,
    watch_id TEXT NOT NULL REFERENCES watches(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL,
    scheduled_at TEXT NOT NULL,
    trigger_reason TEXT NOT NULL,
    definition TEXT NOT NULL,
    watch_run_id INTEGER NOT NULL REFERENCES watch_runs(id),
    status TEXT NOT NULL,
    run_id TEXT,
    session_id TEXT,
    queued_at TEXT NOT NULL,
    deadline_at TEXT NOT NULL,
    settlement_outcome TEXT,
    settlement_summary TEXT
);
CREATE UNIQUE INDEX watch_scheduled_occurrence ON watch_occurrences(watch_id, revision, scheduled_at) WHERE trigger_reason = 'schedule';
CREATE UNIQUE INDEX watch_active_occurrence ON watch_occurrences(watch_id)
WHERE status IN ('pending', 'dispatching', 'running', 'finishing');

CREATE TABLE watch_deliveries (
    occurrence_id TEXT NOT NULL REFERENCES watch_occurrences(id) ON DELETE CASCADE,
    action_key TEXT NOT NULL,
    channel TEXT NOT NULL,
    text TEXT NOT NULL,
    status TEXT NOT NULL,
    slack_ts TEXT,
    error TEXT,
    retry_at TEXT,
    PRIMARY KEY (occurrence_id, action_key)
);
