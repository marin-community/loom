ALTER TABLE sessions ADD COLUMN lifecycle_transition TEXT;
ALTER TABLE sessions ADD COLUMN lifecycle_step TEXT;
ALTER TABLE sessions ADD COLUMN lifecycle_transition_started_at TEXT;
ALTER TABLE sessions ADD COLUMN lifecycle_transition_owner_pid INTEGER;
