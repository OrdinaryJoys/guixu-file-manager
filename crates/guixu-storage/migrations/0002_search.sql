CREATE VIRTUAL TABLE files_fts USING fts5(
    file_id UNINDEXED,
    library_id UNINDEXED,
    name,
    path,
    tokenize = 'unicode61 remove_diacritics 2'
);

INSERT INTO files_fts(file_id,library_id,name,path)
SELECT id,library_id,current_path,current_path
FROM files;

CREATE TRIGGER files_fts_insert AFTER INSERT ON files BEGIN
    INSERT INTO files_fts(file_id,library_id,name,path)
    VALUES(new.id,new.library_id,new.current_path,new.current_path);
END;

CREATE TRIGGER files_fts_update AFTER UPDATE OF current_path,library_id ON files BEGIN
    DELETE FROM files_fts WHERE file_id=old.id;
    INSERT INTO files_fts(file_id,library_id,name,path)
    VALUES(new.id,new.library_id,new.current_path,new.current_path);
END;

CREATE TRIGGER files_fts_delete AFTER DELETE ON files BEGIN
    DELETE FROM files_fts WHERE file_id=old.id;
END;

CREATE TABLE smart_folders (
    id TEXT PRIMARY KEY,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    query TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE(library_id,name)
) STRICT;
