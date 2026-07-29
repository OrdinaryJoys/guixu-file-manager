ALTER TABLE plans ADD COLUMN operation_kind TEXT NOT NULL DEFAULT 'organize'
    CHECK (operation_kind IN ('organize', 'rename', 'move', 'copy', 'trash'));
ALTER TABLE plans ADD COLUMN conflict_policy TEXT NOT NULL DEFAULT 'abort'
    CHECK (conflict_policy IN ('abort', 'skip', 'keep_both'));
ALTER TABLE operations ADD COLUMN operation_kind TEXT NOT NULL DEFAULT 'organize'
    CHECK (operation_kind IN ('organize', 'rename', 'move', 'copy', 'trash'));
