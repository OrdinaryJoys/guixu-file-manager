-- P2：FTS 同步触发器增加 WHEN 守卫，避免路径不变时对 UNINDEXED 列全表扫描。
-- 与 reconcile_file_on 的条件 SET 互补，共同消除无变化重扫的 O(n²) 退化。

DROP TRIGGER files_fts_update;
CREATE TRIGGER files_fts_update AFTER UPDATE OF current_path,library_id ON files
WHEN old.current_path <> new.current_path OR old.library_id <> new.library_id
BEGIN
    DELETE FROM files_fts WHERE file_id=old.id;
    INSERT INTO files_fts(file_id,library_id,name,path)
    VALUES(new.id,new.library_id,new.current_path,new.current_path);
END;

DROP TRIGGER files_fts_trigram_update;
CREATE TRIGGER files_fts_trigram_update AFTER UPDATE OF current_path,library_id ON files
WHEN old.current_path <> new.current_path OR old.library_id <> new.library_id
BEGIN
    DELETE FROM files_fts_trigram WHERE file_id=old.id;
    INSERT INTO files_fts_trigram(file_id,library_id,path)
    VALUES(new.id,new.library_id,new.current_path);
END;
