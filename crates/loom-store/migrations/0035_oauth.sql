-- OAuth clients that may send a person through /oauth/authorize and exchange
-- the result for a Loom credential (the claude.ai connector, say). A
-- confidential client keeps a secret hash; a public client relies on PKCE.
CREATE TABLE oauth_clients (
    client_id     TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    secret_hash   TEXT,
    -- JSON array of the exact redirect URIs codes may be returned to.
    redirect_uris TEXT NOT NULL,
    created_by    TEXT NOT NULL,
    created_at    TEXT NOT NULL
);

-- Single-use authorization codes, kept only as hashes for a few minutes.
CREATE TABLE oauth_codes (
    code_hash      TEXT PRIMARY KEY,
    client_id      TEXT NOT NULL REFERENCES oauth_clients(client_id) ON DELETE CASCADE,
    username       TEXT NOT NULL REFERENCES users(username) ON DELETE CASCADE,
    redirect_uri   TEXT NOT NULL,
    code_challenge TEXT NOT NULL,
    expires_at     TEXT NOT NULL
);

-- Rotating refresh tokens. Each row names the `api_tokens` access token it
-- last issued, so a refresh or a client removal revokes that token too.
-- `expires_at` is fixed when the person consents and survives rotation, so a
-- grant cannot outlive it without a new consent.
CREATE TABLE oauth_refresh_tokens (
    token_hash      TEXT PRIMARY KEY,
    client_id       TEXT NOT NULL REFERENCES oauth_clients(client_id) ON DELETE CASCADE,
    username        TEXT NOT NULL REFERENCES users(username) ON DELETE CASCADE,
    access_token_id TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    expires_at      TEXT NOT NULL
);

CREATE INDEX idx_oauth_refresh_tokens_client ON oauth_refresh_tokens(client_id);
