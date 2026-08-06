CREATE TABLE file_hash_cache (
    file_id TEXT PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    snapshot_size INTEGER NOT NULL CHECK (snapshot_size >= 0),
    snapshot_modified_at_ns INTEGER NOT NULL,
    snapshot_changed_at_ns INTEGER NOT NULL,
    algorithm TEXT NOT NULL,
    quick_fingerprint TEXT NOT NULL,
    content_hash TEXT,
    updated_at_ms INTEGER NOT NULL
) STRICT;

CREATE INDEX file_hash_cache_content
ON file_hash_cache(algorithm, content_hash)
WHERE content_hash IS NOT NULL;
