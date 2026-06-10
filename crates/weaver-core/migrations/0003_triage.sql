-- The triage axis: the overlooker's assessment of a branch, a third status axis
-- distinct from the agent's self-reported `attention`. Two actors never author
-- the same fact — the agent owns `attention`, an overlooker owns `triage`.
--
--   triage_level  '' when unmarked; otherwise ok | attention | blocked
--   triage_note   one-line reason accompanying the mark
--   triage_by     which overlooker (or `manual`) last set it — attribution
--   triage_at     when it was last set; NULL = never marked. Compared against a
--                 session's last activity to render the mark stale once the
--                 session has moved on.
ALTER TABLE branches ADD COLUMN triage_level TEXT NOT NULL DEFAULT '';
ALTER TABLE branches ADD COLUMN triage_note  TEXT NOT NULL DEFAULT '';
ALTER TABLE branches ADD COLUMN triage_by    TEXT NOT NULL DEFAULT '';
ALTER TABLE branches ADD COLUMN triage_at    TEXT;
