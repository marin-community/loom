-- GitHub credentials must come from Loom-owned state. Remove legacy profile
-- and repository environment overrides; per-user Account tokens remain a
-- supported override for ordinary interactive sessions.
DELETE FROM profile_env WHERE name IN ('GH_TOKEN', 'GITHUB_TOKEN');
DELETE FROM repo_env WHERE name IN ('GH_TOKEN', 'GITHUB_TOKEN');
