use std::path::Path;
use std::time::Duration;

use guixu_domain::{FileIdentity, FileSnapshot, JobStatus, OperationStatus};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;

pub const SCHEMA_VERSION: i64 = 5;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("数据库版本 {found} 高于当前程序支持的版本 {supported}")]
    UnsupportedSchema { found: i64, supported: i64 },
    #[error("文件大小超过 SQLite 有符号整数范围")]
    FileTooLarge,
    #[error("任务进度无效")]
    InvalidProgress,
    #[error("搜索条件无效：{0}")]
    InvalidSearch(String),
    #[error("任务不存在或工作进程已经失去租约")]
    LeaseLost,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

pub struct Database {
    connection: Connection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileKind {
    Inserted,
    Unchanged,
    Moved,
    MetadataUpdated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileResult {
    pub file_id: String,
    pub kind: ReconcileKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileObservation {
    pub new_file_id: String,
    pub path: String,
    pub identity: FileIdentity,
    pub snapshot: FileSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationItemIntent {
    pub ordinal: i64,
    pub source_path: String,
    pub target_path: String,
    pub source_identity: FileIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPlanItem {
    pub ordinal: i64,
    pub file_id: String,
    pub source_path: String,
    pub target_path: String,
    pub expected_identity: FileIdentity,
    pub expected_snapshot: FileSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPlanItem {
    pub ordinal: i64,
    pub file_id: Option<String>,
    pub source_path: String,
    pub target_path: String,
    pub expected_identity: FileIdentity,
    pub expected_snapshot: FileSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPlan {
    pub id: String,
    pub library_id: String,
    pub status: String,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub items: Vec<StoredPlanItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredOperationItem {
    pub ordinal: i64,
    pub status: String,
    pub source_path: String,
    pub target_path: String,
    pub source_identity: FileIdentity,
    pub target_identity: Option<FileIdentity>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredOperation {
    pub id: String,
    pub plan_id: String,
    pub library_id: String,
    pub status: String,
    pub created_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub items: Vec<StoredOperationItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryOperation {
    pub id: String,
    pub plan_id: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewJob {
    pub id: String,
    pub kind: String,
    pub payload_json: String,
    pub priority: i64,
    pub progress_total: Option<u64>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedJob {
    pub id: String,
    pub kind: String,
    pub payload_json: String,
    pub progress_current: u64,
    pub progress_total: Option<u64>,
    pub priority: i64,
    pub lease_expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobEvent {
    pub event: String,
    pub detail_json: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFile {
    pub id: String,
    pub current_path: String,
    pub size: u64,
    pub modified_at_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePageCursor {
    pub path: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePage {
    pub items: Vec<IndexedFile>,
    pub next_cursor: Option<FilePageCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryOverview {
    pub total_files: u64,
    pub total_bytes: u64,
    pub latest_modified_at_ns: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryRootRecord {
    pub library_id: String,
    pub name: String,
    pub root_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSummary {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub progress_current: u64,
    pub progress_total: Option<u64>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub file: IndexedFile,
    pub rank: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchSpec {
    pub text: String,
    pub extension: Option<String>,
    pub minimum_size: Option<u64>,
    pub maximum_size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartFolder {
    pub id: String,
    pub library_id: String,
    pub name: String,
    pub query: String,
    pub created_at_ms: i64,
}

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;\nPRAGMA journal_mode = WAL;\nPRAGMA synchronous = FULL;",
        )?;
        let mut database = Self { connection };
        database.migrate()?;
        Ok(database)
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn schema_version(&self) -> Result<i64, StorageError> {
        Ok(self
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    pub fn register_library_root(
        &self,
        library_id: &str,
        root_id: &str,
        name: &str,
        root_path: &str,
        now_ms: i64,
    ) -> Result<LibraryRootRecord, StorageError> {
        let transaction = self.connection.unchecked_transaction()?;
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT libraries.id,libraries.name FROM roots
                 JOIN libraries ON libraries.id=roots.library_id WHERE roots.path=?1 LIMIT 1",
                [root_path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (library_id, name) = if let Some(existing) = existing {
            transaction.execute(
                "UPDATE roots SET enabled=1 WHERE library_id=?1 AND path=?2",
                params![existing.0, root_path],
            )?;
            transaction.execute(
                "UPDATE libraries SET updated_at_ms=?1 WHERE id=?2",
                params![now_ms, existing.0],
            )?;
            existing
        } else {
            transaction.execute(
                "INSERT INTO libraries(id,name,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?3)",
                params![library_id, name, now_ms],
            )?;
            transaction.execute(
                "INSERT INTO roots(id,library_id,path,enabled,created_at_ms) VALUES(?1,?2,?3,1,?4)",
                params![root_id, library_id, root_path, now_ms],
            )?;
            (library_id.to_owned(), name.to_owned())
        };
        transaction.commit()?;
        Ok(LibraryRootRecord {
            library_id,
            name,
            root_path: root_path.to_owned(),
        })
    }

    pub fn latest_library_root(&self) -> Result<Option<LibraryRootRecord>, StorageError> {
        Ok(self
            .connection
            .query_row(
                "SELECT libraries.id,libraries.name,roots.path FROM roots
                 JOIN libraries ON libraries.id=roots.library_id
                 WHERE roots.enabled=1 ORDER BY libraries.updated_at_ms DESC,roots.created_at_ms DESC LIMIT 1",
                [],
                |row| {
                    Ok(LibraryRootRecord {
                        library_id: row.get(0)?,
                        name: row.get(1)?,
                        root_path: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn library_root(
        &self,
        library_id: &str,
    ) -> Result<Option<LibraryRootRecord>, StorageError> {
        Ok(self
            .connection
            .query_row(
                "SELECT libraries.id,libraries.name,roots.path FROM roots
                 JOIN libraries ON libraries.id=roots.library_id
                 WHERE roots.enabled=1 AND libraries.id=?1
                 ORDER BY roots.created_at_ms DESC LIMIT 1",
                [library_id],
                |row| {
                    Ok(LibraryRootRecord {
                        library_id: row.get(0)?,
                        name: row.get(1)?,
                        root_path: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn reconcile_file(
        &mut self,
        library_id: &str,
        new_file_id: &str,
        path: &str,
        identity: &FileIdentity,
        snapshot: &FileSnapshot,
        observed_at_ms: i64,
    ) -> Result<ReconcileResult, StorageError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = reconcile_file_on(
            &transaction,
            library_id,
            new_file_id,
            path,
            identity,
            snapshot,
            observed_at_ms,
        )?;
        transaction.commit()?;
        Ok(result)
    }

    /// 在一个短事务中写入一批流式扫描结果，兼顾吞吐和崩溃恢复边界。
    pub fn reconcile_files_batch(
        &mut self,
        library_id: &str,
        observations: &[FileObservation],
        observed_at_ms: i64,
    ) -> Result<Vec<ReconcileResult>, StorageError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut results = Vec::with_capacity(observations.len());
        for observation in observations {
            results.push(reconcile_file_on(
                &transaction,
                library_id,
                &observation.new_file_id,
                &observation.path,
                &observation.identity,
                &observation.snapshot,
                observed_at_ms,
            )?);
        }
        transaction.commit()?;
        Ok(results)
    }

    pub fn create_plan(
        &self,
        plan_id: &str,
        library_id: &str,
        created_at_ms: i64,
        expires_at_ms: i64,
        items: &[NewPlanItem],
    ) -> Result<(), StorageError> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO plans(id,library_id,status,created_at_ms,expires_at_ms)
             VALUES(?1,?2,'ready',?3,?4)",
            params![plan_id, library_id, created_at_ms, expires_at_ms],
        )?;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO plan_items(
                    plan_id,ordinal,file_id,source_path,target_path,expected_snapshot_json
                 ) VALUES(?1,?2,?3,?4,?5,?6)",
            )?;
            for item in items {
                let expected =
                    serde_json::to_string(&(&item.expected_identity, &item.expected_snapshot))?;
                statement.execute(params![
                    plan_id,
                    item.ordinal,
                    item.file_id,
                    item.source_path,
                    item.target_path,
                    expected,
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn plan(&self, plan_id: &str) -> Result<Option<StoredPlan>, StorageError> {
        let header: Option<(String, String, String, i64, i64)> = self
            .connection
            .query_row(
                "SELECT id,library_id,status,created_at_ms,expires_at_ms FROM plans WHERE id=?1",
                [plan_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, library_id, status, created_at_ms, expires_at_ms)) = header else {
            return Ok(None);
        };
        let mut statement = self.connection.prepare(
            "SELECT ordinal,file_id,source_path,target_path,expected_snapshot_json
             FROM plan_items WHERE plan_id=?1 ORDER BY ordinal",
        )?;
        let rows = statement.query_map([plan_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut items = Vec::new();
        for row in rows {
            let (ordinal, file_id, source_path, target_path, expected_json) = row?;
            let (expected_identity, expected_snapshot) = serde_json::from_str(&expected_json)?;
            items.push(StoredPlanItem {
                ordinal,
                file_id,
                source_path,
                target_path,
                expected_identity,
                expected_snapshot,
            });
        }
        Ok(Some(StoredPlan {
            id,
            library_id,
            status,
            created_at_ms,
            expires_at_ms,
            items,
        }))
    }

    pub fn update_plan_status(&self, plan_id: &str, status: &str) -> Result<bool, StorageError> {
        Ok(self.connection.execute(
            "UPDATE plans SET status=?1 WHERE id=?2",
            params![status, plan_id],
        )? == 1)
    }

    /// 原子写入操作及其全部条目。此事务提交成功后，文件系统执行才可开始。
    pub fn log_operation_intent(
        &mut self,
        operation_id: &str,
        plan_id: &str,
        created_at_ms: i64,
        items: &[OperationItemIntent],
    ) -> Result<(), StorageError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO operations(id,plan_id,status,created_at_ms) VALUES(?1,?2,?3,?4)",
            params![
                operation_id,
                plan_id,
                operation_status_str(OperationStatus::IntentLogged),
                created_at_ms
            ],
        )?;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO operation_items(
                    operation_id,ordinal,status,source_path,target_path,source_identity_json
                 ) VALUES(?1,?2,?3,?4,?5,?6)",
            )?;
            for item in items {
                let identity_json = serde_json::to_string(&item.source_identity)?;
                statement.execute(params![
                    operation_id,
                    item.ordinal,
                    operation_status_str(OperationStatus::IntentLogged),
                    item.source_path,
                    item.target_path,
                    identity_json
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn update_operation_status(
        &self,
        operation_id: &str,
        status: OperationStatus,
        completed_at_ms: Option<i64>,
        error: Option<(&str, &str)>,
    ) -> Result<(), StorageError> {
        let (error_code, error_message) = error
            .map(|(code, message)| (Some(code), Some(message)))
            .unwrap_or((None, None));
        self.connection.execute(
            "UPDATE operations SET status=?1,completed_at_ms=?2,error_code=?3,error_message=?4
             WHERE id=?5",
            params![
                operation_status_str(status),
                completed_at_ms,
                error_code,
                error_message,
                operation_id
            ],
        )?;
        Ok(())
    }

    pub fn update_operation_item(
        &self,
        operation_id: &str,
        ordinal: i64,
        status: OperationStatus,
        target_identity: Option<&FileIdentity>,
        error: Option<(&str, &str)>,
    ) -> Result<(), StorageError> {
        let target_identity_json = target_identity.map(serde_json::to_string).transpose()?;
        let (error_code, error_message) = error
            .map(|(code, message)| (Some(code), Some(message)))
            .unwrap_or((None, None));
        self.connection.execute(
            "UPDATE operation_items SET status=?1,target_identity_json=?2,error_code=?3,error_message=?4
             WHERE operation_id=?5 AND ordinal=?6",
            params![
                operation_status_str(status),
                target_identity_json,
                error_code,
                error_message,
                operation_id,
                ordinal
            ],
        )?;
        Ok(())
    }

    /// 返回启动时必须恢复的非终态操作。
    pub fn recovery_operations(&self) -> Result<Vec<RecoveryOperation>, StorageError> {
        let mut statement = self.connection.prepare(
            "SELECT id,plan_id,status FROM operations
             WHERE status IN ('intent_logged','executing','published','verified','recovery_needed','rollback_pending')
             ORDER BY created_at_ms,id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(RecoveryOperation {
                id: row.get(0)?,
                plan_id: row.get(1)?,
                status: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn operation(&self, operation_id: &str) -> Result<Option<StoredOperation>, StorageError> {
        self.read_operation(
            "SELECT operations.id,operations.plan_id,plans.library_id,operations.status,
                    operations.created_at_ms,operations.completed_at_ms,
                    operations.error_code,operations.error_message
             FROM operations JOIN plans ON plans.id=operations.plan_id
             WHERE operations.id=?1",
            operation_id,
        )
    }

    pub fn operation_history(
        &self,
        library_id: &str,
        requested_limit: usize,
    ) -> Result<Vec<StoredOperation>, StorageError> {
        let limit = i64::try_from(requested_limit.clamp(1, 100)).expect("history limit fits");
        let mut statement = self.connection.prepare(
            "SELECT operations.id FROM operations JOIN plans ON plans.id=operations.plan_id
             WHERE plans.library_id=?1 ORDER BY operations.created_at_ms DESC,operations.id LIMIT ?2",
        )?;
        let ids = statement
            .query_map(params![library_id, limit], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                self.operation(&id)?
                    .ok_or_else(|| StorageError::Sqlite(rusqlite::Error::QueryReturnedNoRows))
            })
            .collect()
    }

    fn read_operation(
        &self,
        query: &str,
        operation_id: &str,
    ) -> Result<Option<StoredOperation>, StorageError> {
        let header: Option<(
            String,
            String,
            String,
            String,
            i64,
            Option<i64>,
            Option<String>,
            Option<String>,
        )> = self
            .connection
            .query_row(query, [operation_id], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            })
            .optional()?;
        let Some((
            id,
            plan_id,
            library_id,
            status,
            created_at_ms,
            completed_at_ms,
            error_code,
            error_message,
        )) = header
        else {
            return Ok(None);
        };
        let mut statement = self.connection.prepare(
            "SELECT ordinal,status,source_path,target_path,source_identity_json,
                    target_identity_json,error_code,error_message
             FROM operation_items WHERE operation_id=?1 ORDER BY ordinal",
        )?;
        let rows = statement.query_map([operation_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?;
        let mut items = Vec::new();
        for row in rows {
            let (
                ordinal,
                status,
                source_path,
                target_path,
                source_json,
                target_json,
                item_error_code,
                item_error_message,
            ) = row?;
            items.push(StoredOperationItem {
                ordinal,
                status,
                source_path,
                target_path,
                source_identity: serde_json::from_str(&source_json)?,
                target_identity: target_json
                    .map(|json| serde_json::from_str(&json))
                    .transpose()?,
                error_code: item_error_code,
                error_message: item_error_message,
            });
        }
        Ok(Some(StoredOperation {
            id,
            plan_id,
            library_id,
            status,
            created_at_ms,
            completed_at_ms,
            error_code,
            error_message,
            items,
        }))
    }

    pub fn enqueue_job(&self, job: &NewJob) -> Result<(), StorageError> {
        let total = job
            .progress_total
            .map(i64::try_from)
            .transpose()
            .map_err(|_| StorageError::InvalidProgress)?;
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO jobs(
                id,kind,status,payload_json,progress_current,progress_total,priority,created_at_ms,updated_at_ms
             ) VALUES(?1,?2,'queued',?3,0,?4,?5,?6,?6)",
            params![
                job.id,
                job.kind,
                job.payload_json,
                total,
                job.priority,
                job.created_at_ms
            ],
        )?;
        insert_job_event(&transaction, &job.id, "queued", None, job.created_at_ms)?;
        transaction.commit()?;
        Ok(())
    }

    /// 先回收过期租约，再按“优先级降序、创建时间升序”原子认领一个任务。
    pub fn claim_next_job(
        &mut self,
        worker_id: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> Result<Option<ClaimedJob>, StorageError> {
        let lease_expires_at_ms = now_ms.saturating_add(lease_duration_ms.max(1));
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        recover_expired_jobs(&transaction, now_ms)?;
        let candidate: Option<(String, String, String, i64, Option<i64>, i64)> = transaction
            .query_row(
                "SELECT id,kind,payload_json,progress_current,progress_total,priority
                 FROM jobs WHERE status='queued'
                 ORDER BY priority DESC,created_at_ms,id LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, kind, payload_json, current, total, priority)) = candidate else {
            transaction.commit()?;
            return Ok(None);
        };
        let updated = transaction.execute(
            "UPDATE jobs SET status='running',lease_owner=?1,lease_expires_at_ms=?2,updated_at_ms=?3
             WHERE id=?4 AND status='queued'",
            params![worker_id, lease_expires_at_ms, now_ms, id],
        )?;
        if updated != 1 {
            return Err(StorageError::LeaseLost);
        }
        insert_job_event(
            &transaction,
            &id,
            "claimed",
            Some(&format!(r#"{{"worker":"{worker_id}"}}"#)),
            now_ms,
        )?;
        transaction.commit()?;
        Ok(Some(ClaimedJob {
            id,
            kind,
            payload_json,
            progress_current: u64::try_from(current).map_err(|_| StorageError::InvalidProgress)?,
            progress_total: total
                .map(u64::try_from)
                .transpose()
                .map_err(|_| StorageError::InvalidProgress)?,
            priority,
            lease_expires_at_ms,
        }))
    }

    pub fn renew_job_lease(
        &self,
        job_id: &str,
        worker_id: &str,
        now_ms: i64,
        lease_duration_ms: i64,
    ) -> Result<(), StorageError> {
        let expires = now_ms.saturating_add(lease_duration_ms.max(1));
        let updated = self.connection.execute(
            "UPDATE jobs SET lease_expires_at_ms=?1,updated_at_ms=?2
             WHERE id=?3 AND lease_owner=?4 AND status IN ('running','pause_requested','cancel_requested')
               AND lease_expires_at_ms>=?2",
            params![expires, now_ms, job_id, worker_id],
        )?;
        ensure_updated(updated)
    }

    pub fn update_job_progress(
        &self,
        job_id: &str,
        worker_id: &str,
        current: u64,
        total: Option<u64>,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        if total.is_some_and(|total| current > total) {
            return Err(StorageError::InvalidProgress);
        }
        let current = i64::try_from(current).map_err(|_| StorageError::InvalidProgress)?;
        let total = total
            .map(i64::try_from)
            .transpose()
            .map_err(|_| StorageError::InvalidProgress)?;
        let updated = self.connection.execute(
            "UPDATE jobs SET progress_current=?1,progress_total=COALESCE(?2,progress_total),updated_at_ms=?3
             WHERE id=?4 AND lease_owner=?5 AND status IN ('running','pause_requested','cancel_requested')
               AND lease_expires_at_ms>=?3",
            params![current, total, now_ms, job_id, worker_id],
        )?;
        ensure_updated(updated)
    }

    pub fn request_job_pause(&self, job_id: &str, now_ms: i64) -> Result<(), StorageError> {
        self.request_job_control(job_id, "pause_requested", "paused", now_ms)
    }

    pub fn request_job_cancel(&self, job_id: &str, now_ms: i64) -> Result<(), StorageError> {
        self.request_job_control(job_id, "cancel_requested", "cancelled", now_ms)
    }

    pub fn resume_job(&self, job_id: &str, now_ms: i64) -> Result<(), StorageError> {
        let transaction = self.connection.unchecked_transaction()?;
        let updated = transaction.execute(
            "UPDATE jobs SET status='queued',progress_current=0,progress_total=NULL,
                    lease_owner=NULL,lease_expires_at_ms=NULL,updated_at_ms=?1
             WHERE id=?2 AND status='paused'",
            params![now_ms, job_id],
        )?;
        ensure_updated(updated)?;
        insert_job_event(&transaction, job_id, "resumed", None, now_ms)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn job_status(&self, job_id: &str) -> Result<Option<JobStatus>, StorageError> {
        let status = self
            .connection
            .query_row("SELECT status FROM jobs WHERE id=?1", [job_id], |row| {
                row.get::<_, String>(0)
            })
            .optional()?;
        status.map(|status| parse_job_status(&status)).transpose()
    }

    fn request_job_control(
        &self,
        job_id: &str,
        running_status: &str,
        inactive_status: &str,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        let transaction = self.connection.unchecked_transaction()?;
        let updated = transaction.execute(
            "UPDATE jobs SET status=CASE WHEN status='running' THEN ?1 ELSE ?2 END,
                    lease_owner=CASE WHEN status='running' THEN lease_owner ELSE NULL END,
                    lease_expires_at_ms=CASE WHEN status='running' THEN lease_expires_at_ms ELSE NULL END,
                    updated_at_ms=?3
             WHERE id=?4 AND status IN ('queued','running','paused')",
            params![running_status, inactive_status, now_ms, job_id],
        )?;
        ensure_updated(updated)?;
        insert_job_event(&transaction, job_id, running_status, None, now_ms)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn acknowledge_job_control(
        &self,
        job_id: &str,
        worker_id: &str,
        requested: JobStatus,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        let (requested_status, final_status, event) = match requested {
            JobStatus::PauseRequested => ("pause_requested", "paused", "paused"),
            JobStatus::CancelRequested => ("cancel_requested", "cancelled", "cancelled"),
            _ => return Err(StorageError::LeaseLost),
        };
        self.finish_owned_job(
            job_id,
            worker_id,
            requested_status,
            final_status,
            event,
            now_ms,
            None,
        )
    }

    pub fn complete_job(
        &self,
        job_id: &str,
        worker_id: &str,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        self.finish_owned_job(
            job_id,
            worker_id,
            "running",
            "completed",
            "completed",
            now_ms,
            None,
        )
    }

    pub fn fail_job(
        &self,
        job_id: &str,
        worker_id: &str,
        now_ms: i64,
        detail_json: Option<&str>,
    ) -> Result<(), StorageError> {
        self.finish_owned_job(
            job_id,
            worker_id,
            "running",
            "failed",
            "failed",
            now_ms,
            detail_json,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_owned_job(
        &self,
        job_id: &str,
        worker_id: &str,
        expected_status: &str,
        final_status: &str,
        event: &str,
        now_ms: i64,
        detail_json: Option<&str>,
    ) -> Result<(), StorageError> {
        let transaction = self.connection.unchecked_transaction()?;
        let updated = transaction.execute(
            "UPDATE jobs SET status=?1,lease_owner=NULL,lease_expires_at_ms=NULL,updated_at_ms=?2
             WHERE id=?3 AND lease_owner=?4 AND status=?5 AND lease_expires_at_ms>=?2",
            params![final_status, now_ms, job_id, worker_id, expected_status],
        )?;
        ensure_updated(updated)?;
        insert_job_event(&transaction, job_id, event, detail_json, now_ms)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn job_events(&self, job_id: &str) -> Result<Vec<JobEvent>, StorageError> {
        let mut statement = self.connection.prepare(
            "SELECT event,detail_json,created_at_ms FROM job_events WHERE job_id=?1 ORDER BY id",
        )?;
        let rows = statement.query_map([job_id], |row| {
            Ok(JobEvent {
                event: row.get(0)?,
                detail_json: row.get(1)?,
                created_at_ms: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// 按路径和稳定记录 ID 做 keyset 分页；不会产生 OFFSET 深分页退化。
    pub fn list_files_page(
        &self,
        library_id: &str,
        after: Option<&FilePageCursor>,
        requested_limit: usize,
    ) -> Result<FilePage, StorageError> {
        let limit = requested_limit.clamp(1, 500);
        let fetch_limit = i64::try_from(limit + 1).expect("page limit fits i64");
        let (after_path, after_id) = after
            .map(|cursor| (cursor.path.as_str(), cursor.id.as_str()))
            .unwrap_or(("", ""));
        let mut statement = self.connection.prepare(
            "SELECT id,current_path,size,modified_at_ns FROM files
             WHERE library_id=?1 AND missing_since_ms IS NULL
               AND (current_path>?2 OR (current_path=?2 AND id>?3))
             ORDER BY current_path,id LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![library_id, after_path, after_id, fetch_limit],
            |row| {
                let size: i64 = row.get(2)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    size,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?;
        let mut items = rows
            .map(|row| {
                let (id, current_path, size, modified_at_ns) = row?;
                Ok(IndexedFile {
                    id,
                    current_path,
                    size: u64::try_from(size).map_err(|_| StorageError::InvalidProgress)?,
                    modified_at_ns,
                })
            })
            .collect::<Result<Vec<_>, StorageError>>()?;
        let has_more = items.len() > limit;
        items.truncate(limit);
        let next_cursor = has_more.then(|| {
            let last = items.last().expect("non-empty limited page");
            FilePageCursor {
                path: last.current_path.clone(),
                id: last.id.clone(),
            }
        });
        Ok(FilePage { items, next_cursor })
    }

    /// 读取资料库的完整统计，不受当前分页或搜索结果影响。
    pub fn library_overview(&self, library_id: &str) -> Result<LibraryOverview, StorageError> {
        let (total_files, total_bytes, latest_modified_at_ns): (i64, i64, Option<i64>) =
            self.connection.query_row(
                "SELECT COUNT(*),COALESCE(SUM(size),0),MAX(modified_at_ns) FROM files
                 WHERE library_id=?1 AND missing_since_ms IS NULL",
                [library_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        Ok(LibraryOverview {
            total_files: u64::try_from(total_files).map_err(|_| StorageError::InvalidProgress)?,
            total_bytes: u64::try_from(total_bytes).map_err(|_| StorageError::InvalidProgress)?,
            latest_modified_at_ns,
        })
    }

    pub fn indexed_file(
        &self,
        library_id: &str,
        file_id: &str,
    ) -> Result<Option<IndexedFile>, StorageError> {
        let row: Option<(String, String, i64, i64)> = self
            .connection
            .query_row(
                "SELECT id,current_path,size,modified_at_ns FROM files
                 WHERE library_id=?1 AND id=?2 AND missing_since_ms IS NULL",
                params![library_id, file_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        row.map(|(id, current_path, size, modified_at_ns)| {
            Ok(IndexedFile {
                id,
                current_path,
                size: u64::try_from(size).map_err(|_| StorageError::InvalidProgress)?,
                modified_at_ns,
            })
        })
        .transpose()
    }

    pub fn list_jobs(&self, limit: usize) -> Result<Vec<JobSummary>, StorageError> {
        let limit = i64::try_from(limit.clamp(1, 200)).expect("job limit fits i64");
        let mut statement = self.connection.prepare(
            "SELECT id,kind,status,progress_current,progress_total,updated_at_ms
             FROM jobs ORDER BY updated_at_ms DESC,id LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        rows.map(|row| {
            let (id, kind, status, current, total, updated_at_ms) = row?;
            Ok(JobSummary {
                id,
                kind,
                status,
                progress_current: u64::try_from(current)
                    .map_err(|_| StorageError::InvalidProgress)?,
                progress_total: total
                    .map(u64::try_from)
                    .transpose()
                    .map_err(|_| StorageError::InvalidProgress)?,
                updated_at_ms,
            })
        })
        .collect()
    }

    pub fn search_files(
        &self,
        library_id: &str,
        query: &str,
        requested_limit: usize,
    ) -> Result<Vec<SearchHit>, StorageError> {
        let spec = parse_search_query(query)?;
        let use_trigram = !spec.text.is_empty()
            && spec
                .text
                .split_whitespace()
                .all(|term| term.chars().count() >= 3);
        let match_query = if use_trigram {
            trigram_query(&spec.text)
        } else {
            fts_query(&spec.text)
        };
        if match_query.is_empty()
            && spec.extension.is_none()
            && spec.minimum_size.is_none()
            && spec.maximum_size.is_none()
        {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(requested_limit.clamp(1, 200)).expect("search limit fits");
        let extension_pattern = spec.extension.map(|extension| format!("%.{extension}"));
        let minimum_size = spec
            .minimum_size
            .map(i64::try_from)
            .transpose()
            .map_err(|_| StorageError::FileTooLarge)?;
        let maximum_size = spec
            .maximum_size
            .map(i64::try_from)
            .transpose()
            .map_err(|_| StorageError::FileTooLarge)?;
        let sql = if match_query.is_empty() {
            "SELECT files.id,files.current_path,files.size,files.modified_at_ns,0.0
             FROM files
             WHERE files.library_id=?1 AND files.missing_since_ms IS NULL
               AND (?2 IS NULL OR lower(files.current_path) LIKE ?2)
               AND (?3 IS NULL OR files.size>=?3) AND (?4 IS NULL OR files.size<=?4)
             ORDER BY files.current_path,files.id LIMIT ?5"
        } else if use_trigram {
            "SELECT files.id,files.current_path,files.size,files.modified_at_ns,bm25(files_fts_trigram)
             FROM files_fts_trigram JOIN files ON files.id=files_fts_trigram.file_id
             WHERE files_fts_trigram MATCH ?1 AND files_fts_trigram.library_id=?2 AND files.missing_since_ms IS NULL
               AND (?3 IS NULL OR lower(files.current_path) LIKE ?3)
               AND (?4 IS NULL OR files.size>=?4) AND (?5 IS NULL OR files.size<=?5)
             ORDER BY bm25(files_fts_trigram),files.current_path,files.id LIMIT ?6"
        } else {
            "SELECT files.id,files.current_path,files.size,files.modified_at_ns,bm25(files_fts,10.0,2.0)
             FROM files_fts JOIN files ON files.id=files_fts.file_id
             WHERE files_fts MATCH ?1 AND files_fts.library_id=?2 AND files.missing_since_ms IS NULL
               AND (?3 IS NULL OR lower(files.current_path) LIKE ?3)
               AND (?4 IS NULL OR files.size>=?4) AND (?5 IS NULL OR files.size<=?5)
             ORDER BY bm25(files_fts,10.0,2.0),files.current_path,files.id LIMIT ?6"
        };
        let mut statement = self.connection.prepare(sql)?;
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, f64>(4)?,
            ))
        };
        let rows = if match_query.is_empty() {
            statement.query_map(
                params![
                    library_id,
                    extension_pattern,
                    minimum_size,
                    maximum_size,
                    limit
                ],
                map_row,
            )?
        } else {
            statement.query_map(
                params![
                    match_query,
                    library_id,
                    extension_pattern,
                    minimum_size,
                    maximum_size,
                    limit
                ],
                map_row,
            )?
        };
        rows.map(|row| {
            let (id, current_path, size, modified_at_ns, rank) = row?;
            Ok(SearchHit {
                file: IndexedFile {
                    id,
                    current_path,
                    size: u64::try_from(size).map_err(|_| StorageError::InvalidProgress)?,
                    modified_at_ns,
                },
                rank,
            })
        })
        .collect()
    }

    pub fn save_smart_folder(&self, folder: &SmartFolder) -> Result<(), StorageError> {
        let spec = parse_search_query(&folder.query)?;
        let rule_json = serde_json::json!({
            "version": 1,
            "text": spec.text,
            "extension": spec.extension,
            "minimumSize": spec.minimum_size,
            "maximumSize": spec.maximum_size,
        })
        .to_string();
        self.connection.execute(
            "INSERT INTO smart_folders(
                id,library_id,name,query,created_at_ms,updated_at_ms,rule_version,rule_json
             ) VALUES(?1,?2,?3,?4,?5,?5,1,?6)
             ON CONFLICT(id) DO UPDATE SET
                name=excluded.name,query=excluded.query,updated_at_ms=excluded.updated_at_ms,
                rule_version=excluded.rule_version,rule_json=excluded.rule_json",
            params![
                folder.id,
                folder.library_id,
                folder.name,
                folder.query,
                folder.created_at_ms,
                rule_json,
            ],
        )?;
        Ok(())
    }

    pub fn list_smart_folders(&self, library_id: &str) -> Result<Vec<SmartFolder>, StorageError> {
        let mut statement = self.connection.prepare(
            "SELECT id,library_id,name,query,created_at_ms FROM smart_folders
             WHERE library_id=?1 ORDER BY name,id",
        )?;
        let rows = statement.query_map([library_id], |row| {
            Ok(SmartFolder {
                id: row.get(0)?,
                library_id: row.get(1)?,
                name: row.get(2)?,
                query: row.get(3)?,
                created_at_ms: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn delete_smart_folder(&self, library_id: &str, id: &str) -> Result<bool, StorageError> {
        Ok(self.connection.execute(
            "DELETE FROM smart_folders WHERE library_id=?1 AND id=?2",
            params![library_id, id],
        )? == 1)
    }

    /// 快照扫描结束后，将本轮没有看到的记录标记为缺失，但保留历史与身份信息。
    pub fn mark_unseen_missing(
        &self,
        library_id: &str,
        directory: &str,
        observed_at_ms: i64,
    ) -> Result<usize, StorageError> {
        Ok(self.connection.execute(
            "UPDATE files SET missing_since_ms=COALESCE(missing_since_ms,?1)
             WHERE library_id=?2 AND last_seen_at_ms<?1
               AND (current_path=?3 OR substr(current_path,1,length(?3)+1)=?3||'/')",
            params![observed_at_ms, library_id, directory],
        )?)
    }

    fn migrate(&mut self) -> Result<(), StorageError> {
        let version = self.schema_version()?;
        if version > SCHEMA_VERSION {
            return Err(StorageError::UnsupportedSchema {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        let mut version = version;
        if version == 0 {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/0001_initial.sql"))?;
            transaction.pragma_update(None, "user_version", 1)?;
            transaction.commit()?;
            version = 1;
        }
        if version == 1 {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/0002_search.sql"))?;
            transaction.pragma_update(None, "user_version", 2)?;
            transaction.commit()?;
            version = 2;
        }
        if version == 2 {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/0003_missing_files.sql"))?;
            transaction.pragma_update(None, "user_version", 3)?;
            transaction.commit()?;
            version = 3;
        }
        if version == 3 {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/0004_trigram_search.sql"))?;
            transaction.pragma_update(None, "user_version", 4)?;
            transaction.commit()?;
            version = 4;
        }
        if version == 4 {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/0005_smart_folder_rules.sql"))?;
            transaction.pragma_update(None, "user_version", 5)?;
            transaction.commit()?;
        }
        Ok(())
    }
}

fn fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .take(12)
        .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn trigram_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .take(12)
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

pub fn parse_search_query(query: &str) -> Result<SearchSpec, StorageError> {
    let mut text = Vec::new();
    let mut extension = None;
    let mut minimum_size = None;
    let mut maximum_size = None;
    let mut size_seen = false;
    for token in query.split_whitespace().take(24) {
        if let Some(value) = token.strip_prefix("ext:") {
            let normalized = value.trim_start_matches('.').to_ascii_lowercase();
            if normalized.is_empty()
                || normalized.len() > 16
                || !normalized
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
                || extension.replace(normalized).is_some()
            {
                return Err(StorageError::InvalidSearch(
                    "ext: 只能指定一个由字母和数字组成的扩展名".to_owned(),
                ));
            }
        } else if let Some(value) = token.strip_prefix("size:") {
            if size_seen {
                return Err(StorageError::InvalidSearch(
                    "当前版本每次搜索只能使用一个 size: 条件".to_owned(),
                ));
            }
            let (minimum, maximum) = parse_size_filter(value)?;
            minimum_size = minimum;
            maximum_size = maximum;
            size_seen = true;
        } else {
            text.push(token);
        }
    }
    if minimum_size.is_some_and(|minimum| maximum_size.is_some_and(|maximum| minimum > maximum)) {
        return Err(StorageError::InvalidSearch("文件大小范围为空".to_owned()));
    }
    Ok(SearchSpec {
        text: text.join(" "),
        extension,
        minimum_size,
        maximum_size,
    })
}

fn parse_size_filter(value: &str) -> Result<(Option<u64>, Option<u64>), StorageError> {
    let (operator, amount) = [">=", "<=", ">", "<", "="]
        .into_iter()
        .find_map(|operator| {
            value
                .strip_prefix(operator)
                .map(|amount| (operator, amount))
        })
        .unwrap_or(("=", value));
    let split = amount
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(amount.len());
    let (number, unit) = amount.split_at(split);
    let number = number.parse::<f64>().map_err(|_| {
        StorageError::InvalidSearch("size: 需要有效数字，例如 size:>10MB".to_owned())
    })?;
    if !number.is_finite() || number < 0.0 {
        return Err(StorageError::InvalidSearch(
            "size: 不能使用负数或非有限数值".to_owned(),
        ));
    }
    let multiplier = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1_f64,
        "kb" => 1024_f64,
        "mb" => 1024_f64.powi(2),
        "gb" => 1024_f64.powi(3),
        "tb" => 1024_f64.powi(4),
        _ => {
            return Err(StorageError::InvalidSearch(
                "size: 仅支持 B、KB、MB、GB、TB".to_owned(),
            ));
        }
    };
    let bytes = number * multiplier;
    if bytes > u64::MAX as f64 {
        return Err(StorageError::FileTooLarge);
    }
    let bytes = bytes.round() as u64;
    if operator == "<" && bytes == 0 {
        return Err(StorageError::InvalidSearch("文件大小范围为空".to_owned()));
    }
    Ok(match operator {
        ">" => (Some(bytes.saturating_add(1)), None),
        ">=" => (Some(bytes), None),
        "<" => (None, Some(bytes.saturating_sub(1))),
        "<=" => (None, Some(bytes)),
        _ => (Some(bytes), Some(bytes)),
    })
}

fn reconcile_file_on(
    connection: &Connection,
    library_id: &str,
    new_file_id: &str,
    path: &str,
    identity: &FileIdentity,
    snapshot: &FileSnapshot,
    observed_at_ms: i64,
) -> Result<ReconcileResult, StorageError> {
    let generation = identity.generation.as_deref().unwrap_or("");
    let existing: Option<(String, String, i64, i64, i64)> = connection
        .query_row(
            "SELECT id,current_path,size,modified_at_ns,changed_at_ns FROM files
             WHERE library_id=?1 AND platform=?2 AND volume_id=?3 AND native_file_id=?4
               AND COALESCE(generation, '')=?5",
            params![
                library_id,
                identity.platform.as_str(),
                identity.volume_id,
                identity.native_file_id,
                generation
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let size = i64::try_from(snapshot.size).map_err(|_| StorageError::FileTooLarge)?;
    if let Some((file_id, old_path, old_size, old_mtime, old_ctime)) = existing {
        let moved = old_path != path;
        let metadata_changed = old_size != size
            || old_mtime != snapshot.modified_at_ns
            || old_ctime != snapshot.changed_at_ns;
        if moved {
            connection.execute(
                "UPDATE file_paths SET valid_until_ms=?1 WHERE file_id=?2 AND valid_until_ms IS NULL",
                params![observed_at_ms, file_id],
            )?;
            connection.execute(
                "INSERT INTO file_paths(file_id,path,valid_from_ms) VALUES(?1,?2,?3)",
                params![file_id, path, observed_at_ms],
            )?;
        }
        connection.execute(
            "UPDATE files SET current_path=?1,size=?2,modified_at_ns=?3,changed_at_ns=?4,
             created_at_ns=?5,last_seen_at_ms=?6,missing_since_ms=NULL WHERE id=?7",
            params![
                path,
                size,
                snapshot.modified_at_ns,
                snapshot.changed_at_ns,
                snapshot.created_at_ns,
                observed_at_ms,
                file_id
            ],
        )?;
        Ok(ReconcileResult {
            file_id,
            kind: if moved {
                ReconcileKind::Moved
            } else if metadata_changed {
                ReconcileKind::MetadataUpdated
            } else {
                ReconcileKind::Unchanged
            },
        })
    } else {
        connection.execute(
            "INSERT INTO files(id,library_id,platform,volume_id,native_file_id,generation,current_path,
             size,modified_at_ns,changed_at_ns,created_at_ns,last_seen_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                new_file_id,
                library_id,
                identity.platform.as_str(),
                identity.volume_id,
                identity.native_file_id,
                identity.generation,
                path,
                size,
                snapshot.modified_at_ns,
                snapshot.changed_at_ns,
                snapshot.created_at_ns,
                observed_at_ms
            ],
        )?;
        connection.execute(
            "INSERT INTO file_paths(file_id,path,valid_from_ms) VALUES(?1,?2,?3)",
            params![new_file_id, path, observed_at_ms],
        )?;
        Ok(ReconcileResult {
            file_id: new_file_id.to_owned(),
            kind: ReconcileKind::Inserted,
        })
    }
}

fn operation_status_str(status: OperationStatus) -> &'static str {
    use OperationStatus::*;
    match status {
        Draft => "draft",
        Planned => "planned",
        PreflightPassed => "preflight_passed",
        IntentLogged => "intent_logged",
        Executing => "executing",
        Published => "published",
        Verified => "verified",
        Completed => "completed",
        Conflict => "conflict",
        Failed => "failed",
        Cancelled => "cancelled",
        RecoveryNeeded => "recovery_needed",
        RollbackPending => "rollback_pending",
        RolledBack => "rolled_back",
    }
}

fn ensure_updated(updated: usize) -> Result<(), StorageError> {
    if updated == 1 {
        Ok(())
    } else {
        Err(StorageError::LeaseLost)
    }
}

fn parse_job_status(status: &str) -> Result<JobStatus, StorageError> {
    match status {
        "queued" => Ok(JobStatus::Queued),
        "running" => Ok(JobStatus::Running),
        "pause_requested" => Ok(JobStatus::PauseRequested),
        "paused" => Ok(JobStatus::Paused),
        "cancel_requested" => Ok(JobStatus::CancelRequested),
        "completed" => Ok(JobStatus::Completed),
        "failed" => Ok(JobStatus::Failed),
        "cancelled" => Ok(JobStatus::Cancelled),
        _ => Err(StorageError::Sqlite(rusqlite::Error::InvalidQuery)),
    }
}

fn insert_job_event(
    connection: &Connection,
    job_id: &str,
    event: &str,
    detail_json: Option<&str>,
    created_at_ms: i64,
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO job_events(job_id,event,detail_json,created_at_ms) VALUES(?1,?2,?3,?4)",
        params![job_id, event, detail_json, created_at_ms],
    )?;
    Ok(())
}

fn recover_expired_jobs(connection: &Connection, now_ms: i64) -> Result<(), rusqlite::Error> {
    connection.execute(
        "UPDATE jobs SET status='paused',lease_owner=NULL,lease_expires_at_ms=NULL,updated_at_ms=?1
         WHERE status='pause_requested' AND lease_expires_at_ms<?1",
        [now_ms],
    )?;
    connection.execute(
        "UPDATE jobs SET status='cancelled',lease_owner=NULL,lease_expires_at_ms=NULL,updated_at_ms=?1
         WHERE status='cancel_requested' AND lease_expires_at_ms<?1",
        [now_ms],
    )?;
    connection.execute(
        "UPDATE jobs SET status='queued',lease_owner=NULL,lease_expires_at_ms=NULL,updated_at_ms=?1
         WHERE status='running' AND lease_expires_at_ms<?1",
        [now_ms],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use guixu_domain::Platform;

    fn identity() -> FileIdentity {
        FileIdentity {
            platform: Platform::MacOs,
            volume_id: "volume-1".to_owned(),
            native_file_id: "inode-42".to_owned(),
            generation: None,
        }
    }

    fn snapshot() -> FileSnapshot {
        FileSnapshot {
            size: 7,
            modified_at_ns: 100,
            changed_at_ns: 101,
            created_at_ns: Some(99),
        }
    }

    #[test]
    fn creates_versioned_database_with_safety_pragmas() {
        let directory = tempfile::tempdir().expect("temp directory");
        let database = Database::open(directory.path().join("library.sqlite3")).expect("open db");
        assert_eq!(
            database.schema_version().expect("schema version"),
            SCHEMA_VERSION
        );
        let journal: String = database
            .connection()
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("journal mode");
        assert_eq!(journal, "wal");
        let foreign_keys: i64 = database
            .connection()
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .expect("foreign keys");
        assert_eq!(foreign_keys, 1);
    }

    #[test]
    fn rejects_database_from_a_newer_application() {
        let connection = Connection::open_in_memory().expect("memory db");
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .expect("set version");
        let mut database = Database { connection };
        assert!(matches!(
            database.migrate(),
            Err(StorageError::UnsupportedSchema { .. })
        ));
    }

    #[test]
    fn rename_reuses_file_record_and_closes_old_path() {
        let directory = tempfile::tempdir().expect("temp directory");
        let mut database =
            Database::open(directory.path().join("library.sqlite3")).expect("open db");
        database
            .connection()
            .execute(
                "INSERT INTO libraries(id,name,created_at_ms,updated_at_ms) VALUES('library','测试',1,1)",
                [],
            )
            .expect("insert library");
        let inserted = database
            .reconcile_file(
                "library",
                "file-1",
                "/tmp/before.txt",
                &identity(),
                &snapshot(),
                10,
            )
            .expect("insert file");
        assert_eq!(inserted.kind, ReconcileKind::Inserted);
        let moved = database
            .reconcile_file(
                "library",
                "unused-new-id",
                "/tmp/after.txt",
                &identity(),
                &snapshot(),
                20,
            )
            .expect("reconcile rename");
        assert_eq!(moved.file_id, "file-1");
        assert_eq!(moved.kind, ReconcileKind::Moved);
        let paths: Vec<(String, Option<i64>)> = database
            .connection()
            .prepare("SELECT path,valid_until_ms FROM file_paths WHERE file_id='file-1' ORDER BY valid_from_ms")
            .expect("prepare history")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query history")
            .collect::<Result<_, _>>()
            .expect("collect history");
        assert_eq!(
            paths,
            vec![
                ("/tmp/before.txt".to_owned(), Some(20)),
                ("/tmp/after.txt".to_owned(), None)
            ]
        );
    }

    #[test]
    fn operation_intent_survives_reopen_and_is_recoverable() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("library.sqlite3");
        {
            let mut database = Database::open(&path).expect("open db");
            database
                .connection()
                .execute(
                    "INSERT INTO libraries(id,name,created_at_ms,updated_at_ms) VALUES('library','测试',1,1)",
                    [],
                )
                .expect("insert library");
            database
                .connection()
                .execute(
                    "INSERT INTO plans(id,library_id,status,created_at_ms,expires_at_ms)
                     VALUES('plan','library','ready',1,999)",
                    [],
                )
                .expect("insert plan");
            database
                .log_operation_intent(
                    "operation",
                    "plan",
                    10,
                    &[OperationItemIntent {
                        ordinal: 0,
                        source_path: "/source.txt".to_owned(),
                        target_path: "/target.txt".to_owned(),
                        source_identity: identity(),
                    }],
                )
                .expect("log durable intent");
        }
        let database = Database::open(&path).expect("reopen db");
        assert_eq!(
            database.recovery_operations().expect("recovery query"),
            vec![RecoveryOperation {
                id: "operation".to_owned(),
                plan_id: "plan".to_owned(),
                status: "intent_logged".to_owned()
            }]
        );
    }

    #[test]
    fn completed_operation_is_removed_from_recovery_queue() {
        let directory = tempfile::tempdir().expect("temp directory");
        let mut database =
            Database::open(directory.path().join("library.sqlite3")).expect("open db");
        database
            .connection()
            .execute_batch(
                "INSERT INTO libraries(id,name,created_at_ms,updated_at_ms) VALUES('library','测试',1,1);
                 INSERT INTO plans(id,library_id,status,created_at_ms,expires_at_ms)
                 VALUES('plan','library','ready',1,999);",
            )
            .expect("fixtures");
        database
            .log_operation_intent("operation", "plan", 10, &[])
            .expect("log intent");
        database
            .update_operation_status("operation", OperationStatus::Completed, Some(20), None)
            .expect("complete operation");
        assert!(
            database
                .recovery_operations()
                .expect("recovery query")
                .is_empty()
        );
    }

    #[test]
    fn plan_and_operation_history_preserve_every_item_after_reopen() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("history.sqlite3");
        {
            let mut database = Database::open(&path).expect("open db");
            database
                .connection()
                .execute(
                    "INSERT INTO libraries(id,name,created_at_ms,updated_at_ms) VALUES('library','测试',1,1)",
                    [],
                )
                .expect("insert library");
            database
                .reconcile_file(
                    "library",
                    "file-1",
                    "/source.txt",
                    &identity(),
                    &snapshot(),
                    5,
                )
                .expect("index plan fixture");
            database
                .create_plan(
                    "plan",
                    "library",
                    10,
                    910,
                    &[NewPlanItem {
                        ordinal: 0,
                        file_id: "file-1".to_owned(),
                        source_path: "/source.txt".to_owned(),
                        target_path: "/archive/source.txt".to_owned(),
                        expected_identity: identity(),
                        expected_snapshot: snapshot(),
                    }],
                )
                .expect("create plan");
            database
                .log_operation_intent(
                    "operation",
                    "plan",
                    20,
                    &[OperationItemIntent {
                        ordinal: 0,
                        source_path: "/source.txt".to_owned(),
                        target_path: "/archive/source.txt".to_owned(),
                        source_identity: identity(),
                    }],
                )
                .expect("log operation");
        }
        let database = Database::open(&path).expect("reopen db");
        let plan = database.plan("plan").expect("read plan").expect("plan");
        assert_eq!(plan.status, "ready");
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].expected_snapshot, snapshot());
        let history = database.operation_history("library", 20).expect("history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].items[0].source_identity, identity());
        assert_eq!(history[0].status, "intent_logged");
    }

    fn job(id: &str, priority: i64, created_at_ms: i64) -> NewJob {
        NewJob {
            id: id.to_owned(),
            kind: "scan".to_owned(),
            payload_json: "{}".to_owned(),
            priority,
            progress_total: Some(10),
            created_at_ms,
        }
    }

    #[test]
    fn job_queue_claims_highest_priority_then_oldest() {
        let directory = tempfile::tempdir().expect("temp directory");
        let mut database = Database::open(directory.path().join("jobs.sqlite3")).expect("open db");
        database.enqueue_job(&job("low", 10, 1)).expect("low job");
        database
            .enqueue_job(&job("new-high", 100, 3))
            .expect("new high job");
        database
            .enqueue_job(&job("old-high", 100, 2))
            .expect("old high job");

        let first = database
            .claim_next_job("worker", 10, 100)
            .expect("claim")
            .expect("job");
        assert_eq!(first.id, "old-high");
        database
            .complete_job(&first.id, "worker", 11)
            .expect("complete");
        let second = database
            .claim_next_job("worker", 12, 100)
            .expect("claim")
            .expect("job");
        assert_eq!(second.id, "new-high");
    }

    #[test]
    fn expired_worker_lease_returns_job_to_queue() {
        let directory = tempfile::tempdir().expect("temp directory");
        let mut database = Database::open(directory.path().join("jobs.sqlite3")).expect("open db");
        database.enqueue_job(&job("scan", 100, 1)).expect("enqueue");
        database
            .claim_next_job("dead-worker", 10, 5)
            .expect("first claim")
            .expect("job");
        let recovered = database
            .claim_next_job("new-worker", 16, 10)
            .expect("recovery claim")
            .expect("job");
        assert_eq!(recovered.id, "scan");
        assert!(matches!(
            database.update_job_progress("scan", "dead-worker", 1, Some(10), 17),
            Err(StorageError::LeaseLost)
        ));
    }

    #[test]
    fn progress_and_pause_are_cooperative_and_durable() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("jobs.sqlite3");
        {
            let mut database = Database::open(&path).expect("open db");
            database.enqueue_job(&job("scan", 100, 1)).expect("enqueue");
            database
                .claim_next_job("worker", 10, 100)
                .expect("claim")
                .expect("job");
            database
                .update_job_progress("scan", "worker", 4, Some(10), 11)
                .expect("progress");
            database
                .request_job_pause("scan", 12)
                .expect("pause request");
            database
                .acknowledge_job_control("scan", "worker", JobStatus::PauseRequested, 13)
                .expect("pause acknowledgement");
        }
        let database = Database::open(&path).expect("reopen db");
        let state: (String, i64, Option<String>) = database
            .connection()
            .query_row(
                "SELECT status,progress_current,lease_owner FROM jobs WHERE id='scan'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("job state");
        assert_eq!(state, ("paused".to_owned(), 4, None));
        let events = database.job_events("scan").expect("events");
        assert_eq!(
            events
                .iter()
                .map(|event| event.event.as_str())
                .collect::<Vec<_>>(),
            vec!["queued", "claimed", "pause_requested", "paused"]
        );
        assert_eq!(
            database.job_status("scan").expect("job status"),
            Some(JobStatus::Paused)
        );
        database.resume_job("scan", 14).expect("resume job");
        assert_eq!(
            database.job_status("scan").expect("resumed status"),
            Some(JobStatus::Queued)
        );
        assert_eq!(
            database
                .job_events("scan")
                .expect("resumed events")
                .last()
                .map(|event| event.event.as_str()),
            Some("resumed")
        );
    }

    #[test]
    fn invalid_progress_and_stale_completion_are_rejected() {
        let directory = tempfile::tempdir().expect("temp directory");
        let mut database = Database::open(directory.path().join("jobs.sqlite3")).expect("open db");
        database.enqueue_job(&job("scan", 100, 1)).expect("enqueue");
        database
            .claim_next_job("worker", 10, 5)
            .expect("claim")
            .expect("job");
        assert!(matches!(
            database.update_job_progress("scan", "worker", 11, Some(10), 11),
            Err(StorageError::InvalidProgress)
        ));
        assert!(matches!(
            database.complete_job("scan", "worker", 16),
            Err(StorageError::LeaseLost)
        ));
    }

    #[test]
    fn file_pages_use_stable_cursor_without_duplicates() {
        let directory = tempfile::tempdir().expect("temp directory");
        let mut database = Database::open(directory.path().join("index.sqlite3")).expect("open db");
        database
            .connection()
            .execute(
                "INSERT INTO libraries(id,name,created_at_ms,updated_at_ms) VALUES('library','测试',1,1)",
                [],
            )
            .expect("insert library");
        for (index, path) in ["/c.txt", "/a.txt", "/b.txt"].iter().enumerate() {
            let mut file_identity = identity();
            file_identity.native_file_id = format!("inode-{index}");
            database
                .reconcile_file(
                    "library",
                    &format!("file-{index}"),
                    path,
                    &file_identity,
                    &snapshot(),
                    10,
                )
                .expect("index file");
        }
        let first = database
            .list_files_page("library", None, 2)
            .expect("first page");
        assert_eq!(
            first
                .items
                .iter()
                .map(|file| file.current_path.as_str())
                .collect::<Vec<_>>(),
            vec!["/a.txt", "/b.txt"]
        );
        let second = database
            .list_files_page("library", first.next_cursor.as_ref(), 2)
            .expect("second page");
        assert_eq!(second.items[0].current_path, "/c.txt");
        assert!(second.next_cursor.is_none());
    }

    #[test]
    fn fts_search_handles_unicode_prefixes_and_special_syntax_safely() {
        let directory = tempfile::tempdir().expect("temp directory");
        let mut database =
            Database::open(directory.path().join("search.sqlite3")).expect("open db");
        database
            .connection()
            .execute(
                "INSERT INTO libraries(id,name,created_at_ms,updated_at_ms) VALUES('library','测试',1,1)",
                [],
            )
            .expect("insert library");
        database
            .reconcile_file(
                "library",
                "file-project",
                "/资料/项目计划.md",
                &identity(),
                &snapshot(),
                10,
            )
            .expect("index file");
        let hits = database
            .search_files("library", "项目", 20)
            .expect("unicode search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file.id, "file-project");
        let substring_hits = database
            .search_files("library", "目计划", 20)
            .expect("trigram substring search");
        assert_eq!(substring_hits.len(), 1);
        assert_eq!(substring_hits[0].file.id, "file-project");
        let filtered = database
            .search_files("library", "ext:md size:>=7B", 20)
            .expect("metadata-only search");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].file.id, "file-project");
        assert!(
            database
                .search_files("library", "项目 ext:pdf", 20)
                .expect("combined search")
                .is_empty()
        );
        assert!(database.search_files("library", "\" OR * -", 20).is_ok());
    }

    #[test]
    fn structured_search_parser_validates_extensions_and_binary_sizes() {
        assert_eq!(
            parse_search_query("合同 ext:PDF size:>10MB").expect("structured query"),
            SearchSpec {
                text: "合同".to_owned(),
                extension: Some("pdf".to_owned()),
                minimum_size: Some(10 * 1024 * 1024 + 1),
                maximum_size: None,
            }
        );
        assert!(matches!(
            parse_search_query("ext:pdf ext:txt"),
            Err(StorageError::InvalidSearch(_))
        ));
        assert!(matches!(
            parse_search_query("size:huge"),
            Err(StorageError::InvalidSearch(_))
        ));
        assert!(matches!(
            parse_search_query("size:<0B"),
            Err(StorageError::InvalidSearch(_))
        ));
    }

    #[test]
    fn library_overview_counts_the_full_active_snapshot() {
        let directory = tempfile::tempdir().expect("temp directory");
        let mut database =
            Database::open(directory.path().join("overview.sqlite3")).expect("open db");
        database
            .connection()
            .execute(
                "INSERT INTO libraries(id,name,created_at_ms,updated_at_ms) VALUES('library','测试',1,1)",
                [],
            )
            .expect("insert library");
        for (index, size) in [7_u64, 11_u64].into_iter().enumerate() {
            let mut file_identity = identity();
            file_identity.native_file_id = format!("overview-inode-{index}");
            let mut file_snapshot = snapshot();
            file_snapshot.size = size;
            file_snapshot.modified_at_ns = 100 + index as i64;
            database
                .reconcile_file(
                    "library",
                    &format!("overview-file-{index}"),
                    &format!("/tmp/overview-{index}.txt"),
                    &file_identity,
                    &file_snapshot,
                    10,
                )
                .expect("index overview fixture");
        }
        assert_eq!(
            database.library_overview("library").expect("overview"),
            LibraryOverview {
                total_files: 2,
                total_bytes: 18,
                latest_modified_at_ns: Some(101),
            }
        );
    }

    #[test]
    fn smart_folders_are_scoped_to_their_library() {
        let directory = tempfile::tempdir().expect("temp directory");
        let database = Database::open(directory.path().join("smart.sqlite3")).expect("open db");
        database
            .connection()
            .execute(
                "INSERT INTO libraries(id,name,created_at_ms,updated_at_ms) VALUES('library','测试',1,1)",
                [],
            )
            .expect("insert library");
        let folder = SmartFolder {
            id: "smart-1".to_owned(),
            library_id: "library".to_owned(),
            name: "项目资料".to_owned(),
            query: "项目".to_owned(),
            created_at_ms: 10,
        };
        database
            .save_smart_folder(&folder)
            .expect("save smart folder");
        assert_eq!(
            database
                .list_smart_folders("library")
                .expect("list smart folders"),
            vec![folder]
        );
        let stored_rule: (i64, String) = database
            .connection()
            .query_row(
                "SELECT rule_version,rule_json FROM smart_folders WHERE id='smart-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("stored smart rule");
        assert_eq!(stored_rule.0, 1);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stored_rule.1).expect("rule json")["text"],
            "项目"
        );
        assert!(
            database
                .delete_smart_folder("library", "smart-1")
                .expect("delete smart folder")
        );
    }

    #[test]
    fn upgrades_a_version_one_database_without_losing_files() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("upgrade.sqlite3");
        {
            let connection = Connection::open(&path).expect("open v1 db");
            connection
                .execute_batch(include_str!("../migrations/0001_initial.sql"))
                .expect("create v1 schema");
            connection
                .pragma_update(None, "user_version", 1)
                .expect("mark v1");
            connection
                .execute(
                    "INSERT INTO libraries(id,name,created_at_ms,updated_at_ms) VALUES('library','测试',1,1)",
                    [],
                )
                .expect("insert library");
        }
        let database = Database::open(&path).expect("upgrade database");
        assert_eq!(database.schema_version().expect("version"), SCHEMA_VERSION);
        let smart_table: i64 = database
            .connection()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='smart_folders'",
                [],
                |row| row.get(0),
            )
            .expect("migration table");
        assert_eq!(smart_table, 1);
    }
}
