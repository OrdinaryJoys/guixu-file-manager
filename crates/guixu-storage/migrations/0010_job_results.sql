CREATE TABLE job_results (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
    result_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
) STRICT;
