ALTER TABLE users ADD COLUMN authorization_kind TEXT NOT NULL DEFAULT 'manual'
    CHECK (authorization_kind IN ('manual', 'github_organization'));
ALTER TABLE users ADD COLUMN authorization_github_org_id INTEGER;
ALTER TABLE users ADD COLUMN authorization_github_org_login TEXT;
ALTER TABLE users ADD COLUMN authorization_valid_until TEXT
    CHECK (
        (authorization_kind = 'manual'
            AND authorization_github_org_id IS NULL
            AND authorization_github_org_login IS NULL
            AND authorization_valid_until IS NULL)
        OR
        (authorization_kind = 'github_organization'
            AND github_login IS NOT NULL
            AND github_user_id IS NOT NULL
            AND authorization_github_org_id IS NOT NULL
            AND authorization_github_org_login IS NOT NULL
            AND authorization_valid_until IS NOT NULL)
    );

CREATE UNIQUE INDEX users_github_user_id_unique
    ON users(github_user_id) WHERE github_user_id IS NOT NULL;
