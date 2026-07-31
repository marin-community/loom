ALTER TABLE automation_runs ADD COLUMN channel TEXT;

CREATE TABLE automation_channels (
    actor_subject TEXT NOT NULL,
    source        TEXT NOT NULL,
    service_tag   TEXT NOT NULL,
    profile       TEXT NOT NULL REFERENCES profiles(name),
    channel       TEXT NOT NULL,
    owner_run_id  TEXT NOT NULL REFERENCES automation_runs(id),
    session_id    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    PRIMARY KEY (actor_subject, source, service_tag, profile, channel)
);
