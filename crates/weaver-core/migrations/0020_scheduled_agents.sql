ALTER TABLE watches ADD COLUMN agent_spec TEXT;
ALTER TABLE watches ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE watches ADD COLUMN deployment_managed INTEGER NOT NULL DEFAULT 0;
ALTER TABLE watches ADD COLUMN paused INTEGER NOT NULL DEFAULT 0;
ALTER TABLE watches ADD COLUMN run_timeout_secs INTEGER NOT NULL DEFAULT 300;
ALTER TABLE watches ADD COLUMN state_version INTEGER NOT NULL DEFAULT 0;

ALTER TABLE watch_runs ADD COLUMN execution_owner TEXT;

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
    settlement_summary TEXT,
    automatic INTEGER NOT NULL DEFAULT 1,
    dry_run INTEGER NOT NULL DEFAULT 0,
    trigger_context TEXT NOT NULL DEFAULT '{}',
    script_process TEXT
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
