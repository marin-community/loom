-- Artifacts can be archived: hidden from every default listing, still readable
-- by name and id so links, embeds, reviews, and review threads keep working.
-- NULL is active; `artifacts.restore` clears it. Nothing is copied or pruned.
ALTER TABLE artifacts ADD COLUMN archived_at TEXT;
