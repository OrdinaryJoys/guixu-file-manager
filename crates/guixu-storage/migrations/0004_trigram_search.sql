CREATE VIRTUAL TABLE files_fts_trigram USING fts5(
    file_id UNINDEXED,
    library_id UNINDEXED,
    path,
    tokenize = 'trigram'
);

INSERT INTO files_fts_trigram(file_id,library_id,path)
SELECT id,library_id,current_path
FROM files;

CREATE TRIGGER files_fts_trigram_insert AFTER INSERT ON files BEGIN
    INSERT INTO files_fts_trigram(file_id,library_id,path)
    VALUES(new.id,new.library_id,new.current_path);
END;

CREATE TRIGGER files_fts_trigram_update AFTER UPDATE OF current_path,library_id ON files BEGIN
    DELETE FROM files_fts_trigram WHERE file_id=old.id;
    INSERT INTO files_fts_trigram(file_id,library_id,path)
    VALUES(new.id,new.library_id,new.current_path);
END;

CREATE TRIGGER files_fts_trigram_delete AFTER DELETE ON files BEGIN
    DELETE FROM files_fts_trigram WHERE file_id=old.id;
END;
