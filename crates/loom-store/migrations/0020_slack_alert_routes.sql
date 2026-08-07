-- Slack threads an automation delivery pointed at a session.
--
-- Distinct from the single-valued `slack` branch tag, which fixes one thread as
-- a session's status-card home. One long-lived operator session triages many
-- alerts, each announced in its own thread, so the thread-to-session relation is
-- many-to-one and cannot live on that tag. Keyed on the thread rather than the
-- branch: the lookup that matters is inbound — a mention arrives carrying a
-- channel and a thread, and has to find the session that owns it.
CREATE TABLE slack_routes (
    channel_id TEXT NOT NULL,
    thread_ts  TEXT NOT NULL,
    branch_id  TEXT NOT NULL REFERENCES branches(id) ON DELETE CASCADE,
    -- The run source that opened the route (`grafana`, `ops`, `actions`), for
    -- attribution in logs and the settings pane.
    source     TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (channel_id, thread_ts)
);

CREATE INDEX idx_slack_routes_branch ON slack_routes(branch_id, updated_at);
