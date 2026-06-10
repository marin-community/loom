-- Tags: a general per-branch annotation, collapsing the agent's `attention`
-- self-report and an overlooker's `triage` assessment into one mechanism. One
-- row per `(branch_id, key)`, single-valued and upserted. The well-known keys
-- live in `weaver-core/src/tags.rs`:
--
--   attention  author: the agent.        Stored values: attention | blocked. Loud.
--   triage     author: an overlooker.    Stored values: attention | blocked. Loud.
--   <other>    free-form, quiet, single-valued.
--
-- Absence is the calm/default state: there is no stored `ok`. Setting attention
-- to `ok` deletes the row, which structurally enforces "ok ⇒ no badge". The
-- branch's prose status message stays on `branches.description`; only the level
-- moves here.
--
--   value    the level/payload (e.g. `attention` | `blocked` for loud keys)
--   note     one-line reason accompanying the tag
--   set_by   who set it — `agent`, an overlooker name, or `manual`. Attribution.
--   set_at   when it was last set. Compared against a session's last activity to
--            render the tag stale once the session has moved past it.
CREATE TABLE IF NOT EXISTS tags (
    branch_id TEXT NOT NULL REFERENCES branches(id) ON DELETE CASCADE,
    key       TEXT NOT NULL,
    value     TEXT NOT NULL DEFAULT '',
    note      TEXT NOT NULL DEFAULT '',
    set_by    TEXT NOT NULL DEFAULT '',
    set_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (branch_id, key)
);
CREATE INDEX IF NOT EXISTS idx_tags_key ON tags(key, value);

-- Move existing non-ok marks into the new table. `ok` marks are dropped: under
-- the tags model absence is the calm state, so an `ok` level becomes no row.
INSERT INTO tags (branch_id, key, value, note, set_by, set_at)
  SELECT id, 'attention', attention, '', 'agent', updated_at
  FROM branches WHERE attention IN ('attention','blocked');
INSERT INTO tags (branch_id, key, value, note, set_by, set_at)
  SELECT id, 'triage', triage_level, triage_note, triage_by, COALESCE(triage_at, updated_at)
  FROM branches WHERE triage_level IN ('attention','blocked');

-- Retire the columns the tags table replaces. `description` stays on `branches`.
ALTER TABLE branches DROP COLUMN attention;
ALTER TABLE branches DROP COLUMN triage_level;
ALTER TABLE branches DROP COLUMN triage_note;
ALTER TABLE branches DROP COLUMN triage_by;
ALTER TABLE branches DROP COLUMN triage_at;
