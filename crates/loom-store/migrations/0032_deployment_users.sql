ALTER TABLE users ADD COLUMN deployment_managed INTEGER NOT NULL DEFAULT 0
    CHECK (deployment_managed IN (0, 1));
