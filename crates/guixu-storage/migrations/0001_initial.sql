CREATE TABLE libraries (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE roots (
    id TEXT PRIMARY KEY,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    UNIQUE(library_id, path)
) STRICT;

CREATE TABLE files (
    id TEXT PRIMARY KEY,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    platform TEXT,
    volume_id TEXT,
    native_file_id TEXT,
    generation TEXT,
    current_path TEXT NOT NULL,
    size INTEGER NOT NULL CHECK (size >= 0),
    modified_at_ns INTEGER NOT NULL,
    changed_at_ns INTEGER NOT NULL,
    created_at_ns INTEGER,
    quick_fingerprint TEXT,
    content_hash TEXT,
    last_seen_at_ms INTEGER NOT NULL,
    UNIQUE(library_id, platform, volume_id, native_file_id, generation)
) STRICT;
CREATE INDEX files_library_path ON files(library_id, current_path);
CREATE INDEX files_library_size ON files(library_id, size);

CREATE TABLE file_paths (
    file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    valid_from_ms INTEGER NOT NULL,
    valid_until_ms INTEGER,
    PRIMARY KEY(file_id, path, valid_from_ms)
) STRICT;

CREATE TABLE plans (
    id TEXT PRIMARY KEY,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    status TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE plan_items (
    plan_id TEXT NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    file_id TEXT REFERENCES files(id) ON DELETE SET NULL,
    source_path TEXT NOT NULL,
    target_path TEXT NOT NULL,
    expected_snapshot_json TEXT NOT NULL,
    PRIMARY KEY(plan_id, ordinal)
) STRICT;

CREATE TABLE operations (
    id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL REFERENCES plans(id),
    status TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER,
    error_code TEXT,
    error_message TEXT
) STRICT;

CREATE TABLE operation_items (
    operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    status TEXT NOT NULL,
    source_path TEXT NOT NULL,
    target_path TEXT NOT NULL,
    source_identity_json TEXT NOT NULL,
    target_identity_json TEXT,
    error_code TEXT,
    error_message TEXT,
    PRIMARY KEY(operation_id, ordinal)
) STRICT;

CREATE TABLE jobs (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    progress_current INTEGER NOT NULL DEFAULT 0,
    progress_total INTEGER,
    priority INTEGER NOT NULL DEFAULT 100,
    lease_owner TEXT,
    lease_expires_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;
CREATE INDEX jobs_claimable ON jobs(status, priority DESC, created_at_ms);

CREATE TABLE job_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    event TEXT NOT NULL,
    detail_json TEXT,
    created_at_ms INTEGER NOT NULL
) STRICT;
