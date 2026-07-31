CREATE TABLE session_metadata_assistance (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    title_generation_enabled INTEGER NOT NULL DEFAULT 1,
    title_generation_status TEXT NOT NULL DEFAULT 'idle',
    cue_source_cursor TEXT,
    cue_text TEXT,
    cue_generated_at TEXT,
    cue_evidence TEXT NOT NULL DEFAULT '[]',
    updated_at TEXT NOT NULL
);
