CREATE TABLE remote_mcp_servers (
    identity              TEXT PRIMARY KEY,
    group_name            TEXT NOT NULL,
    label                 TEXT NOT NULL,
    description           TEXT NOT NULL DEFAULT '',
    enabled               INTEGER NOT NULL DEFAULT 1,
    current_revision      INTEGER NOT NULL DEFAULT 1,
    managed_by_deployment INTEGER NOT NULL DEFAULT 0,
    created_at            TEXT NOT NULL,
    updated_at            TEXT NOT NULL
);

CREATE TABLE remote_mcp_revisions (
    identity    TEXT NOT NULL REFERENCES remote_mcp_servers(identity) ON DELETE CASCADE,
    revision    INTEGER NOT NULL,
    url         TEXT NOT NULL,
    auth_json   TEXT NOT NULL,
    digest      TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (identity, revision)
);
