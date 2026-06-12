-- Issue tags: a free-form `(key, value)` label on an issue, mirroring the
-- per-branch `tags` table (0005) but keyed on `issue_id`. One row per
-- `(issue_id, key)`, single-valued and upserted.
--
-- Unlike branch tags, issue tags carry no loud `attention`/`triage` ladder —
-- they are purely quiet annotations (priority, area, kind, …) rendered as
-- deletable pills. Every key is free-form; a value must be non-empty (clearing
-- a label deletes the row, same as branch tags).
--
--   value    the label payload, e.g. `high` for a `priority` key
--   note     one-line reason accompanying the tag
--   set_by   who set it — an agent, an overlooker name, or `manual`. Attribution.
--   set_at   when it was last set (ISO-8601).
--
-- Foreign keys are not enabled on the pool, so the issue-delete path clears an
-- issue's tags explicitly (see `weaver_core::issue::delete`); the FK reference
-- here records intent and keeps the schema honest.
CREATE TABLE IF NOT EXISTS issue_tags (
    issue_id INTEGER NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    key      TEXT NOT NULL,
    value    TEXT NOT NULL DEFAULT '',
    note     TEXT NOT NULL DEFAULT '',
    set_by   TEXT NOT NULL DEFAULT '',
    set_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (issue_id, key)
);
CREATE INDEX IF NOT EXISTS idx_issue_tags_key ON issue_tags(key, value);
