CREATE TABLE file_features (
    file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    feature_kind TEXT NOT NULL,
    model_version TEXT NOT NULL,
    snapshot_size INTEGER NOT NULL CHECK(snapshot_size >= 0),
    snapshot_modified_at_ns INTEGER NOT NULL,
    snapshot_changed_at_ns INTEGER NOT NULL,
    dimensions INTEGER NOT NULL CHECK(dimensions > 0),
    quantization TEXT NOT NULL,
    feature_blob BLOB NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(file_id, feature_kind, model_version)
) STRICT;

CREATE TABLE similarity_runs (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    algorithm_version TEXT NOT NULL,
    thresholds_json TEXT NOT NULL,
    benchmark_profile TEXT NOT NULL,
    completed_at_ms INTEGER
) STRICT;

CREATE TABLE similarity_edges (
    run_id TEXT NOT NULL REFERENCES similarity_runs(job_id) ON DELETE CASCADE,
    left_file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    right_file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    evidence_kind TEXT NOT NULL,
    raw_distance REAL NOT NULL,
    calibrated_score REAL NOT NULL,
    explanation_json TEXT NOT NULL,
    PRIMARY KEY(run_id, left_file_id, right_file_id, evidence_kind),
    CHECK(left_file_id < right_file_id)
) STRICT;

CREATE INDEX similarity_edges_score ON similarity_edges(run_id, calibrated_score DESC);
