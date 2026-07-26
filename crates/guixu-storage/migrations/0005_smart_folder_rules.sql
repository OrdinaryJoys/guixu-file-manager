ALTER TABLE smart_folders ADD COLUMN rule_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE smart_folders ADD COLUMN rule_json TEXT;
