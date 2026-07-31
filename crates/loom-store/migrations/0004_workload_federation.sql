ALTER TABLE profile_env ADD COLUMN source TEXT NOT NULL DEFAULT 'literal';
ALTER TABLE profile_env ADD COLUMN secret_ref TEXT;
ALTER TABLE profiles ADD COLUMN managed_by_deployment INTEGER NOT NULL DEFAULT 0;
ALTER TABLE automation_runs ADD COLUMN service_tag TEXT NOT NULL DEFAULT 'unknown';
UPDATE automation_runs SET service_tag = source;

ALTER TABLE federation_mappings RENAME TO federation_mappings_legacy;

CREATE TABLE federation_mappings (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL UNIQUE,
    provider          TEXT NOT NULL,
    issuer            TEXT NOT NULL,
    audience          TEXT NOT NULL,
    subject           TEXT,
    service_account   TEXT,
    service_tag       TEXT NOT NULL,
    repository_id     TEXT,
    workflow_ref      TEXT,
    event_name        TEXT,
    ref_pattern       TEXT,
    profiles_json     TEXT NOT NULL,
    managed_by_deployment INTEGER NOT NULL DEFAULT 0,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

INSERT INTO federation_mappings (
    id, name, provider, issuer, audience, subject, service_account,
    service_tag, repository_id, workflow_ref, event_name, ref_pattern,
    profiles_json, managed_by_deployment, created_at, updated_at
)
SELECT
    id,
    'github-' || id,
    'github',
    issuer,
    audience,
    NULL,
    NULL,
    'github-actions',
    repository_id,
    workflow_ref,
    event_name,
    ref_pattern,
    json_array(profile),
    0,
    created_at,
    created_at
FROM federation_mappings_legacy;

DROP TABLE federation_mappings_legacy;

CREATE INDEX idx_federation_lookup
    ON federation_mappings(issuer, audience, provider);
