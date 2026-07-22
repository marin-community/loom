ALTER TABLE api_tokens ADD COLUMN grant_json TEXT NOT NULL DEFAULT '{"kind":"admin"}';
ALTER TABLE api_tokens ADD COLUMN subject TEXT;
ALTER TABLE api_tokens ADD COLUMN bound_session_id TEXT;
CREATE INDEX idx_api_tokens_bound_session ON api_tokens(bound_session_id);

CREATE TABLE automation_runs (
    id               TEXT PRIMARY KEY,
    actor_subject    TEXT NOT NULL,
    source           TEXT NOT NULL,
    profile          TEXT NOT NULL REFERENCES profiles(name),
    idempotency_key  TEXT NOT NULL,
    request_json     TEXT NOT NULL,
    session_id       TEXT NOT NULL,
    status           TEXT NOT NULL,
    outcome          TEXT,
    summary          TEXT NOT NULL DEFAULT '',
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    UNIQUE(actor_subject, idempotency_key)
);
CREATE INDEX idx_automation_runs_session ON automation_runs(session_id);

CREATE TABLE federation_mappings (
    id               TEXT PRIMARY KEY,
    issuer           TEXT NOT NULL,
    audience         TEXT NOT NULL,
    repository_id    TEXT NOT NULL,
    workflow_ref     TEXT NOT NULL,
    event_name       TEXT,
    ref_pattern      TEXT,
    profile          TEXT NOT NULL REFERENCES profiles(name),
    created_at       TEXT NOT NULL,
    UNIQUE(issuer, audience, repository_id, workflow_ref)
);
