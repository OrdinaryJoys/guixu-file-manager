ALTER TABLE files ADD COLUMN missing_since_ms INTEGER;
CREATE INDEX files_library_missing ON files(library_id,missing_since_ms,current_path);
