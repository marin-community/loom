ALTER TABLE profiles ADD COLUMN prelude TEXT NOT NULL DEFAULT 'weaver';
ALTER TABLE profiles ADD COLUMN restricted INTEGER NOT NULL DEFAULT 0;
ALTER TABLE profiles ADD COLUMN allowed_tools TEXT NOT NULL DEFAULT '[]';

ALTER TABLE sessions ADD COLUMN policy_prelude TEXT NOT NULL DEFAULT 'weaver';
ALTER TABLE sessions ADD COLUMN policy_restricted INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN policy_allowed_tools TEXT NOT NULL DEFAULT '[]';

INSERT INTO profiles (
    name, description, agent_kind, model, effort, protocol, mode, class,
    strict, env_clear, ambient_allowlist, idle_archive_secs, max_concurrent,
    turn_budget, revision, created_at, updated_at, prelude, restricted,
    allowed_tools
) VALUES (
    'github_comment',
    'Restricted GitHub comment automation with a caller-supplied task prompt',
    'claude', '', '', 'acp', 'default', 'automation',
    1, 1, '[]', 900, 4, 4, 1,
    strftime('%Y-%m-%dT%H:%M:%fZ','now'),
    strftime('%Y-%m-%dT%H:%M:%fZ','now'),
    'none', 1,
    json_array(
        'Read(./**)',
        'mcp__loom_github__issue_view',
        'mcp__loom_github__issue_comment',
        'mcp__loom_github__issue_edit',
        'mcp__loom_github__pr_view',
        'mcp__loom_github__pr_comment',
        'mcp__loom_github__pr_edit'
    )
) ON CONFLICT(name) DO UPDATE SET
    description=excluded.description,
    agent_kind=excluded.agent_kind,
    model=excluded.model,
    effort=excluded.effort,
    protocol=excluded.protocol,
    mode=excluded.mode,
    class=excluded.class,
    strict=excluded.strict,
    env_clear=excluded.env_clear,
    ambient_allowlist=excluded.ambient_allowlist,
    idle_archive_secs=excluded.idle_archive_secs,
    max_concurrent=excluded.max_concurrent,
    turn_budget=excluded.turn_budget,
    revision=profiles.revision + 1,
    updated_at=excluded.updated_at,
    prelude=excluded.prelude,
    restricted=excluded.restricted,
    allowed_tools=excluded.allowed_tools;
