-- The Overlooker subsystem: periodic / triggered watch programs over the fleet.
--
-- `overlookers` is one configured watch definition; `overlooker_runs` is the
-- append-only audit of each execution ("round"). The live tmux/session runtime
-- stays single-owner in the daemon — these tables only describe *what* to watch
-- and *what happened*, never a second runtime.
--
-- `trigger_spec` is JSON (avoids the SQLite `TRIGGER` keyword as a column name):
--   {"cron":"0 * * * *"}            scheduled — the timer emits a cron tick
--   {"every":"30m"}                 interval sugar over a crontab
--   {"event":"attention","level":"blocked"}   reactive — match a stream event
-- plus an optional "repo" key pinning the overlooker to one repository.
-- `scope` is the JSON fleet query the round surveys (e.g. {"attention":"!ok"}).
-- `program` is `builtin:<name>` (a stock program) or a path under
-- `~/.weaver/overlookers/`. `capabilities` is a JSON array drawn from the
-- intervention ladder (observe/mark/escalate/nudge/interrupt/launch).
CREATE TABLE IF NOT EXISTS overlookers (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL UNIQUE,
    enabled         INTEGER NOT NULL DEFAULT 0,
    trigger_spec    TEXT NOT NULL DEFAULT '{}',
    scope           TEXT NOT NULL DEFAULT '{}',
    program         TEXT NOT NULL DEFAULT 'builtin:status',
    params          TEXT NOT NULL DEFAULT '{}',
    capabilities    TEXT NOT NULL DEFAULT '["observe","mark","escalate"]',
    model           TEXT NOT NULL DEFAULT '',
    effort          TEXT NOT NULL DEFAULT '',
    cooldown_secs   INTEGER NOT NULL DEFAULT 0,
    last_run_at     TEXT,
    next_run_at     TEXT,
    warm_session_id TEXT,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

-- One row per round. `outcome` is ok | noop | skipped | error; `actions` is a
-- JSON array of the marks/nudges/etc. the round took (the audit the safety
-- story and the panel depend on).
CREATE TABLE IF NOT EXISTS overlooker_runs (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    overlooker_id   TEXT NOT NULL,
    trigger_reason  TEXT NOT NULL DEFAULT '',
    started_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    finished_at     TEXT,
    outcome         TEXT NOT NULL DEFAULT '',
    summary         TEXT NOT NULL DEFAULT '',
    actions         TEXT NOT NULL DEFAULT '[]',
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_overlooker_runs ON overlooker_runs(overlooker_id, id);
