-- ACP adapters advertise their composer controls at runtime. Keep the latest
-- complete snapshot so a provider exit or loom restart does not reduce an
-- inspectable conversation to an empty, "dumb" composer.
CREATE TABLE session_acp_metadata (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    metadata   TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
