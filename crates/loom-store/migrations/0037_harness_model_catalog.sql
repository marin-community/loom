CREATE TABLE harness_model_catalog (
    harness TEXT NOT NULL,
    model_id TEXT NOT NULL,
    label TEXT NOT NULL DEFAULT '',
    options_json TEXT,
    adapter_version TEXT NOT NULL DEFAULT '',
    credential_fingerprint TEXT NOT NULL DEFAULT '',
    fetched_at TEXT,
    PRIMARY KEY (harness, model_id)
);

ALTER TABLE profiles ADD COLUMN fast TEXT NOT NULL DEFAULT '';
