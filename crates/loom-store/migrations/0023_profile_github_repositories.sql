ALTER TABLE profiles
ADD COLUMN github_repositories TEXT NOT NULL DEFAULT '[]';

ALTER TABLE sessions
ADD COLUMN policy_github_repositories TEXT NOT NULL DEFAULT '[]';
