ALTER TABLE profiles ADD COLUMN mcp_access TEXT NOT NULL
    DEFAULT '{"mode":"none","groups":[]}';
ALTER TABLE profiles ADD COLUMN mcp_policy TEXT NOT NULL DEFAULT '';

ALTER TABLE sessions ADD COLUMN policy_mcp_access TEXT NOT NULL
    DEFAULT '{"selection":{"mode":"none","groups":[]},"capability_sets":[]}';

-- Reclassify the one MCP capability set supported before this migration.
-- Operators could copy the stock profile, so translate every profile that
-- selected it rather than special-casing the stock name.
UPDATE profiles
SET mcp_access = '{"mode":"groups","groups":["github"]}'
WHERE EXISTS (
    SELECT 1 FROM json_each(
        CASE WHEN json_valid(profiles.allowed_tools)
             THEN profiles.allowed_tools ELSE '[]' END
    )
    WHERE value IN ('mcp/github/comment', 'mcp/github/comment@v1')
);

UPDATE profiles
SET allowed_tools = (
    SELECT json_group_array(value)
    FROM json_each(
        CASE WHEN json_valid(profiles.allowed_tools)
             THEN profiles.allowed_tools ELSE '[]' END
    )
    WHERE value NOT IN ('mcp/github/comment', 'mcp/github/comment@v1')
)
WHERE EXISTS (
    SELECT 1 FROM json_each(
        CASE WHEN json_valid(profiles.allowed_tools)
             THEN profiles.allowed_tools ELSE '[]' END
    )
    WHERE value IN ('mcp/github/comment', 'mcp/github/comment@v1')
);

CREATE TABLE custom_mcp_servers (
    identity          TEXT PRIMARY KEY,
    group_name        TEXT NOT NULL,
    label             TEXT NOT NULL,
    description       TEXT NOT NULL DEFAULT '',
    enabled           INTEGER NOT NULL DEFAULT 1,
    current_revision  INTEGER NOT NULL DEFAULT 1,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

CREATE TABLE custom_mcp_revisions (
    identity            TEXT NOT NULL REFERENCES custom_mcp_servers(identity) ON DELETE CASCADE,
    revision            INTEGER NOT NULL,
    source              TEXT NOT NULL,
    test_source         TEXT NOT NULL DEFAULT '',
    digest              TEXT NOT NULL,
    tools_json          TEXT NOT NULL DEFAULT '[]',
    validation_state    TEXT NOT NULL,
    validation_message  TEXT NOT NULL DEFAULT '',
    created_at          TEXT NOT NULL,
    PRIMARY KEY (identity, revision)
);
