use std::collections::HashSet;
use std::ffi::{CString, OsStr};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use guixu_domain::{FileIdentity, FileOperationKind, FileSnapshot, OperationStatus};
use guixu_platform::{ObserveError, observe_file};
use guixu_storage::{Database, OperationItemIntent, StorageError, StoredOperation};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

/// 故障注入点：`GUIXU_FAULT_POINT=<name>` 匹配时在此循环等待（等待测试 kill -9）。
/// 仅在 `fault-injection` feature 下生效；生产构建为零开销空函数。
#[cfg(feature = "fault-injection")]
pub fn fault_point(name: &str) {
    if std::env::var("GUIXU_FAULT_POINT").as_deref() == Ok(name) {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(not(feature = "fault-injection"))]
pub fn fault_point(_name: &str) {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveIntent {
    pub root: PathBuf,
    pub source: PathBuf,
    pub target: PathBuf,
    pub expected_identity: FileIdentity,
    pub expected_snapshot: FileSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameRequest {
    pub file_id: String,
    pub source: PathBuf,
    pub new_name: String,
    pub expected_identity: FileIdentity,
    pub expected_snapshot: FileSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRename {
    pub file_id: String,
    pub source: PathBuf,
    pub target: PathBuf,
    pub expected_identity: FileIdentity,
    pub expected_snapshot: FileSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyRequest {
    pub file_id: String,
    pub source: PathBuf,
    pub expected_identity: FileIdentity,
    pub expected_snapshot: FileSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCopy {
    pub file_id: String,
    pub source: PathBuf,
    pub target: PathBuf,
    pub expected_identity: FileIdentity,
    pub expected_snapshot: FileSnapshot,
}

#[derive(Debug, Error)]
pub enum CopyPlanError {
    #[error("复制计划不能为空")]
    Empty,
    #[error("目标必须是资料库内的普通文件夹")]
    InvalidDestination,
    #[error("多个文件会复制到同一个目标：{target}")]
    DuplicateTarget { target: String },
    #[error("文件 {file_id} 未通过复制预检：{source}")]
    Preflight {
        file_id: String,
        #[source]
        source: PreflightError,
    },
    #[error("无法解析授权路径：{0}")]
    Io(#[from] io::Error),
}

/// 生成只读复制计划。目标目录必须已存在、不是符号链接且位于授权资料库内。
pub fn plan_copies(
    root: &Path,
    destination: &Path,
    requests: &[CopyRequest],
) -> Result<Vec<PlannedCopy>, CopyPlanError> {
    if requests.is_empty() {
        return Err(CopyPlanError::Empty);
    }
    let canonical_root = root.canonicalize()?;
    let destination_metadata = fs::symlink_metadata(destination)?;
    if !destination_metadata.is_dir() || destination_metadata.file_type().is_symlink() {
        return Err(CopyPlanError::InvalidDestination);
    }
    let canonical_destination = destination.canonicalize()?;
    if !contained(&canonical_destination, &canonical_root) {
        return Err(CopyPlanError::InvalidDestination);
    }

    let mut reserved_targets = HashSet::with_capacity(requests.len());
    let mut planned = Vec::with_capacity(requests.len());
    for request in requests {
        let source = request
            .source
            .canonicalize()
            .map_err(|error| CopyPlanError::Preflight {
                file_id: request.file_id.clone(),
                source: PreflightError::Io(error),
            })?;
        let file_name = source
            .file_name()
            .ok_or(CopyPlanError::InvalidDestination)?;
        let target = canonical_destination.join(file_name);
        let normalized_target = target
            .to_string_lossy()
            .nfc()
            .flat_map(char::to_lowercase)
            .collect::<String>();
        if !reserved_targets.insert(normalized_target) {
            return Err(CopyPlanError::DuplicateTarget {
                target: target.to_string_lossy().into_owned(),
            });
        }
        preflight(&MoveIntent {
            root: canonical_root.clone(),
            source: source.clone(),
            target: target.clone(),
            expected_identity: request.expected_identity.clone(),
            expected_snapshot: request.expected_snapshot.clone(),
        })
        .map_err(|source| CopyPlanError::Preflight {
            file_id: request.file_id.clone(),
            source,
        })?;
        planned.push(PlannedCopy {
            file_id: request.file_id.clone(),
            source,
            target,
            expected_identity: request.expected_identity.clone(),
            expected_snapshot: request.expected_snapshot.clone(),
        });
    }
    planned.sort_by(|left, right| left.source.cmp(&right.source));
    Ok(planned)
}

#[derive(Debug, Error)]
pub enum TrashPlanError {
    #[error("废纸篓计划不能为空")]
    Empty,
    #[error("内部隔离区无效或已被替换")]
    InvalidQuarantine,
    #[error("文件 {file_id} 未通过废纸篓预检：{source}")]
    Preflight {
        file_id: String,
        #[source]
        source: PreflightError,
    },
    #[error("无法解析授权路径：{0}")]
    Io(#[from] io::Error),
    #[error("计划中存在重复的废纸篓目标：{target}")]
    DuplicateTarget { target: String },
}

/// 将原相对路径保存在每个计划的独立隔离命名空间中，避免同名文件互相冲突。
pub fn plan_trash(
    root: &Path,
    namespace: &str,
    requests: &[CopyRequest],
) -> Result<Vec<PlannedCopy>, TrashPlanError> {
    if requests.is_empty() {
        return Err(TrashPlanError::Empty);
    }
    validate_file_name(namespace).map_err(|_| TrashPlanError::InvalidQuarantine)?;
    let canonical_root = root.canonicalize()?;
    let quarantine_root = canonical_root.join(".guixu-trash");
    if quarantine_root.exists() {
        let metadata = fs::symlink_metadata(&quarantine_root)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(TrashPlanError::InvalidQuarantine);
        }
    }
    let namespace_root = quarantine_root.join(namespace);
    let mut reserved_targets = HashSet::with_capacity(requests.len());
    let mut planned = Vec::with_capacity(requests.len());
    for request in requests {
        let source = request
            .source
            .canonicalize()
            .map_err(|error| TrashPlanError::Preflight {
                file_id: request.file_id.clone(),
                source: PreflightError::Io(error),
            })?;
        let relative =
            source
                .strip_prefix(&canonical_root)
                .map_err(|_| TrashPlanError::Preflight {
                    file_id: request.file_id.clone(),
                    source: PreflightError::OutsideRoot,
                })?;
        let target = namespace_root.join(relative);
        if !reserved_targets.insert(
            target
                .to_string_lossy()
                .nfc()
                .flat_map(char::to_lowercase)
                .collect::<String>(),
        ) {
            return Err(TrashPlanError::DuplicateTarget {
                target: target.to_string_lossy().into_owned(),
            });
        }
        preflight(&MoveIntent {
            root: canonical_root.clone(),
            source: source.clone(),
            target: target.clone(),
            expected_identity: request.expected_identity.clone(),
            expected_snapshot: request.expected_snapshot.clone(),
        })
        .map_err(|source| TrashPlanError::Preflight {
            file_id: request.file_id.clone(),
            source,
        })?;
        planned.push(PlannedCopy {
            file_id: request.file_id.clone(),
            source,
            target,
            expected_identity: request.expected_identity.clone(),
            expected_snapshot: request.expected_snapshot.clone(),
        });
    }
    planned.sort_by(|left, right| left.source.cmp(&right.source));
    Ok(planned)
}

#[derive(Debug, Error)]
pub enum RenamePlanError {
    #[error("重命名计划不能为空")]
    Empty,
    #[error("文件 {file_id} 的新名称无效：{reason}")]
    InvalidName {
        file_id: String,
        reason: &'static str,
    },
    #[error("文件 {file_id} 的新名称与当前名称相同")]
    Unchanged { file_id: String },
    #[error("多个文件会重命名为同一个目标：{target}")]
    DuplicateTarget { target: String },
    #[error("文件 {file_id} 未通过重命名预检：{source}")]
    Preflight {
        file_id: String,
        #[source]
        source: PreflightError,
    },
    #[error("无法解析授权资料库：{0}")]
    Io(#[from] io::Error),
}

/// 生成只读重命名计划。任一条目失败时不返回部分计划，也不写入磁盘。
pub fn plan_renames(
    root: &Path,
    requests: &[RenameRequest],
) -> Result<Vec<PlannedRename>, RenamePlanError> {
    if requests.is_empty() {
        return Err(RenamePlanError::Empty);
    }
    let canonical_root = root.canonicalize()?;
    let mut reserved_targets = HashSet::with_capacity(requests.len());
    let mut planned = Vec::with_capacity(requests.len());

    for request in requests {
        validate_file_name(&request.new_name).map_err(|reason| RenamePlanError::InvalidName {
            file_id: request.file_id.clone(),
            reason,
        })?;
        let source = request
            .source
            .canonicalize()
            .map_err(|error| RenamePlanError::Preflight {
                file_id: request.file_id.clone(),
                source: PreflightError::Io(error),
            })?;
        if !contained(&source, &canonical_root) {
            return Err(RenamePlanError::Preflight {
                file_id: request.file_id.clone(),
                source: PreflightError::OutsideRoot,
            });
        }
        let parent = source
            .parent()
            .ok_or_else(|| RenamePlanError::InvalidName {
                file_id: request.file_id.clone(),
                reason: "源文件没有可用的父目录",
            })?;
        let target = parent.join(&request.new_name);
        if source == target {
            return Err(RenamePlanError::Unchanged {
                file_id: request.file_id.clone(),
            });
        }
        let normalized_target = target
            .to_string_lossy()
            .nfc()
            .flat_map(char::to_lowercase)
            .collect::<String>();
        if !reserved_targets.insert(normalized_target) {
            return Err(RenamePlanError::DuplicateTarget {
                target: target.to_string_lossy().into_owned(),
            });
        }
        preflight(&MoveIntent {
            root: canonical_root.clone(),
            source: source.clone(),
            target: target.clone(),
            expected_identity: request.expected_identity.clone(),
            expected_snapshot: request.expected_snapshot.clone(),
        })
        .map_err(|source| RenamePlanError::Preflight {
            file_id: request.file_id.clone(),
            source,
        })?;
        planned.push(PlannedRename {
            file_id: request.file_id.clone(),
            source,
            target,
            expected_identity: request.expected_identity.clone(),
            expected_snapshot: request.expected_snapshot.clone(),
        });
    }
    planned.sort_by(|left, right| left.source.cmp(&right.source));
    Ok(planned)
}

fn validate_file_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("名称不能为空");
    }
    if name.trim() != name {
        return Err("名称首尾不能包含空白字符");
    }
    if name.len() > 255 {
        return Err("名称超过 255 字节限制");
    }
    let candidate = Path::new(name);
    if candidate.file_name() != Some(OsStr::new(name)) || candidate.components().count() != 1 {
        return Err("名称不能包含路径分隔符或相对路径");
    }
    if name
        .chars()
        .any(|character| character.is_control() || r#"<>:"/\|?*"#.contains(character))
    {
        return Err("名称包含跨平台不安全字符");
    }
    if name.ends_with('.') {
        return Err("名称不能以句点结尾");
    }
    let base = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let reserved = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4
            && matches!(&base[..3], "COM" | "LPT")
            && matches!(base.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        return Err("名称是系统保留名称");
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum PreflightError {
    #[error("源文件或目标目录越过授权根目录")]
    OutsideRoot,
    #[error("目标路径已经存在")]
    TargetExists,
    #[error("源文件身份已经变化")]
    IdentityChanged,
    #[error("源文件内容或元数据已经变化")]
    SnapshotChanged,
    #[error(transparent)]
    Observe(#[from] ObserveError),
    #[error("无法解析授权路径：{0}")]
    Io(#[from] std::io::Error),
}

pub fn preflight(intent: &MoveIntent) -> Result<(), PreflightError> {
    let root = intent.root.canonicalize()?;
    let source = intent.source.canonicalize()?;
    let target_parent = intent.target.parent().ok_or(PreflightError::OutsideRoot)?;
    if !intent.target.is_absolute() || !intent.target.starts_with(&intent.root) {
        return Err(PreflightError::OutsideRoot);
    }
    let target_ancestor = canonical_existing_ancestor(target_parent)?;
    if !contained(&source, &root) || !contained(&target_ancestor, &root) {
        return Err(PreflightError::OutsideRoot);
    }
    if intent.target.exists() {
        // 仅大小写或 Unicode 规范化形式不同的重命名：目标与源是同一文件。
        if let (Ok(canonical_source), Ok(canonical_target)) =
            (source.canonicalize(), intent.target.canonicalize())
        {
            if canonical_source != canonical_target {
                return Err(PreflightError::TargetExists);
            }
            // 大小写/规范化改名：允许继续。
        } else {
            return Err(PreflightError::TargetExists);
        }
    }
    let observed = observe_file(&source)?;
    if observed.identity != intent.expected_identity {
        return Err(PreflightError::IdentityChanged);
    }
    if observed.snapshot != intent.expected_snapshot {
        return Err(PreflightError::SnapshotChanged);
    }
    Ok(())
}

fn canonical_existing_ancestor(path: &Path) -> io::Result<PathBuf> {
    let mut candidate = path;
    loop {
        match candidate.canonicalize() {
            Ok(canonical) => return Ok(canonical),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                candidate = candidate.parent().ok_or(error)?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn contained(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    AtomicRename,
    CrossVolumeCopy,
    VerifiedCopy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveOutcome {
    pub transfer: TransferKind,
    pub target_identity: FileIdentity,
    pub target_snapshot: FileSnapshot,
    pub content_hash: [u8; 32],
}

#[derive(Debug, Error)]
pub enum MoveError {
    #[error(transparent)]
    Preflight(#[from] PreflightError),
    #[error("目标路径已经存在")]
    TargetExists,
    #[error("执行后的文件身份或内容校验失败：{0}")]
    Verification(&'static str),
    #[error("目标已经发布，但源文件尚未删除，需要恢复处理：{0}")]
    PublishedSourceRetained(io::Error),
    #[error(transparent)]
    Observe(#[from] ObserveError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionReport {
    pub operation_id: String,
    pub completed_items: usize,
    pub total_items: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryAudit {
    pub not_started: usize,
    pub completed: usize,
    pub needs_attention: usize,
}

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("整理计划不存在")]
    PlanNotFound,
    #[error("整理计划当前状态不可执行：{0}")]
    PlanNotReady(String),
    #[error("整理计划已经过期")]
    PlanExpired,
    #[error("整理计划没有可执行条目")]
    EmptyPlan,
    #[error("当前执行器不支持操作类型：{0}")]
    UnsupportedOperationKind(String),
    #[error("操作记录不存在")]
    OperationNotFound,
    #[error("当前操作状态不可撤销：{0}")]
    OperationNotUndoable(String),
    #[error("操作条目缺少已发布文件身份")]
    MissingPublishedIdentity,
    #[error("操作条目缺少已发布文件校验信息")]
    MissingPublishedVerification,
    #[error("操作执行到第 {ordinal} 项失败：{message}")]
    ItemFailed { ordinal: i64, message: String },
    #[error(transparent)]
    Move(#[from] MoveError),
    #[error(transparent)]
    Preflight(#[from] PreflightError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// 在任何磁盘修改前校验全部条目并持久化操作意图，再逐项安全移动。
pub fn execute_stored_plan(
    database: &mut Database,
    root: &Path,
    plan_id: &str,
    operation_id: &str,
    now_ms: i64,
) -> Result<ExecutionReport, WorkflowError> {
    execute_stored_plan_with_hook(database, root, plan_id, operation_id, now_ms, |_, _| Ok(()))
}

fn execute_stored_plan_with_hook(
    database: &mut Database,
    root: &Path,
    plan_id: &str,
    operation_id: &str,
    now_ms: i64,
    mut before_item: impl FnMut(i64, &MoveIntent) -> Result<(), MoveError>,
) -> Result<ExecutionReport, WorkflowError> {
    let plan = database.plan(plan_id)?.ok_or(WorkflowError::PlanNotFound)?;
    if plan.status != "ready" {
        return Err(WorkflowError::PlanNotReady(plan.status));
    }
    if plan.expires_at_ms < now_ms {
        database.update_plan_status(plan_id, "expired")?;
        return Err(WorkflowError::PlanExpired);
    }
    if plan.items.is_empty() {
        return Err(WorkflowError::EmptyPlan);
    }
    if !matches!(
        plan.operation_kind,
        FileOperationKind::Organize
            | FileOperationKind::Rename
            | FileOperationKind::Move
            | FileOperationKind::Copy
            | FileOperationKind::Trash
    ) {
        return Err(WorkflowError::UnsupportedOperationKind(
            plan.operation_kind.as_str().to_owned(),
        ));
    }
    let intents = plan
        .items
        .iter()
        .map(|item| MoveIntent {
            root: root.to_owned(),
            source: PathBuf::from(&item.source_path),
            target: PathBuf::from(&item.target_path),
            expected_identity: item.expected_identity.clone(),
            expected_snapshot: item.expected_snapshot.clone(),
        })
        .collect::<Vec<_>>();
    for intent in &intents {
        preflight(intent)?;
    }
    database.log_operation_intent(
        operation_id,
        plan_id,
        now_ms,
        &plan
            .items
            .iter()
            .map(|item| OperationItemIntent {
                ordinal: item.ordinal,
                source_path: item.source_path.clone(),
                target_path: item.target_path.clone(),
                source_identity: item.expected_identity.clone(),
            })
            .collect::<Vec<_>>(),
    )?;
    database.update_plan_status(plan_id, "executing")?;
    database.update_operation_status(operation_id, OperationStatus::Executing, None, None)?;
    fault_point("after_intent");

    let mut completed = 0;
    for (item, intent) in plan.items.iter().zip(&intents) {
        let result = (|| -> Result<MoveOutcome, MoveError> {
            before_item(item.ordinal, intent)?;
            let parent = intent.target.parent().ok_or_else(|| {
                MoveError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "target has no parent",
                ))
            })?;
            fs::create_dir_all(parent)?;
            preflight(intent)?;
            database
                .update_operation_item(
                    operation_id,
                    item.ordinal,
                    OperationStatus::Executing,
                    None,
                    None,
                )
                .map_err(|error| MoveError::Io(io::Error::other(error.to_string())))?;
            if plan.operation_kind == FileOperationKind::Copy {
                execute_copy(intent)
            } else {
                execute_move(intent)
            }
        })();
        match result {
            Ok(outcome) => {
                database.record_published_operation_item(
                    operation_id,
                    item.ordinal,
                    OperationStatus::Completed,
                    &outcome.target_identity,
                    &outcome.target_snapshot,
                    &hex_hash(outcome.content_hash),
                )?;
                fault_point("after_publish");
                if plan.operation_kind == FileOperationKind::Trash {
                    database.mark_file_missing(&plan.library_id, &item.source_path, now_ms)?;
                } else {
                    let indexed_file_id = if plan.operation_kind == FileOperationKind::Copy {
                        format!("copy-{operation_id}-{}", item.ordinal)
                    } else {
                        item.file_id
                            .clone()
                            .unwrap_or_else(|| format!("moved-{operation_id}-{}", item.ordinal))
                    };
                    database.reconcile_file(
                        &plan.library_id,
                        &indexed_file_id,
                        &item.target_path,
                        &outcome.target_identity,
                        &outcome.target_snapshot,
                        now_ms,
                    )?;
                }
                completed += 1;
            }
            Err(error) => {
                let message = error.to_string();
                database.update_operation_item(
                    operation_id,
                    item.ordinal,
                    OperationStatus::Failed,
                    None,
                    Some(("file_operation_failed", &message)),
                )?;
                let status = if completed == 0 {
                    OperationStatus::Failed
                } else {
                    OperationStatus::RecoveryNeeded
                };
                database.update_operation_status(
                    operation_id,
                    status,
                    Some(now_ms),
                    Some(("file_operation_failed", &message)),
                )?;
                return Err(WorkflowError::ItemFailed {
                    ordinal: item.ordinal,
                    message,
                });
            }
        }
    }
    database.update_operation_status(
        operation_id,
        OperationStatus::Completed,
        Some(now_ms),
        None,
    )?;
    database.update_plan_status(plan_id, "completed")?;
    Ok(ExecutionReport {
        operation_id: operation_id.to_owned(),
        completed_items: completed,
        total_items: plan.items.len(),
    })
}

/// 仅当所有已发布目标仍是本次操作产生的文件、且原路径空闲时才开始撤销。
pub fn undo_stored_operation(
    database: &mut Database,
    root: &Path,
    operation_id: &str,
    now_ms: i64,
) -> Result<ExecutionReport, WorkflowError> {
    let operation = database
        .operation(operation_id)?
        .ok_or(WorkflowError::OperationNotFound)?;
    // P1：接受 completed 和 recovery_needed（只回滚已完成的条目）。
    let is_recovery = operation.status == "recovery_needed";
    if operation.status != "completed" && !is_recovery {
        return Err(WorkflowError::OperationNotUndoable(operation.status));
    }
    if operation.operation_kind == FileOperationKind::Copy {
        return undo_copy_operation(database, root, &operation, now_ms);
    }
    let mut reverse = Vec::with_capacity(operation.items.len());
    for item in operation.items.iter().rev() {
        // recovery_needed 模式下只回滚已完成/已发布的条目，跳过失败条目。
        if is_recovery
            && !matches!(
                item.status.as_str(),
                "completed" | "published" | "verified" | "rollback_pending"
            )
        {
            continue;
        }
        let published_identity = item
            .target_identity
            .as_ref()
            .ok_or(WorkflowError::MissingPublishedIdentity)?;
        // P1 三层验证：身份 + 快照 + 可选内容哈希。
        validate_undo_target(
            Path::new(&item.target_path),
            published_identity,
            item.target_snapshot.as_ref(),
            item.content_hash.as_deref(),
        )?;
        if Path::new(&item.source_path).exists() {
            return Err(WorkflowError::Move(MoveError::TargetExists));
        }
        let observed = observe_file(&item.target_path).map_err(MoveError::Observe)?;
        let intent = MoveIntent {
            root: root.to_owned(),
            source: PathBuf::from(&item.target_path),
            target: PathBuf::from(&item.source_path),
            expected_identity: observed.identity,
            expected_snapshot: observed.snapshot,
        };
        preflight(&intent)?;
        reverse.push((item, intent));
    }
    database.update_operation_status(operation_id, OperationStatus::RollbackPending, None, None)?;
    for (item, _) in &reverse {
        database.update_operation_item(
            operation_id,
            item.ordinal,
            OperationStatus::RollbackPending,
            item.target_identity.as_ref(),
            None,
        )?;
    }
    let mut completed = 0;
    for (item, intent) in reverse {
        let parent = intent.target.parent().ok_or_else(|| {
            WorkflowError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "undo target has no parent",
            ))
        })?;
        fs::create_dir_all(parent)?;
        preflight(&intent)?;
        match execute_move(&intent) {
            Ok(outcome) => {
                database.update_operation_item(
                    operation_id,
                    item.ordinal,
                    OperationStatus::RolledBack,
                    Some(&outcome.target_identity),
                    None,
                )?;
                // 使用源文件身份计算稳定记录 id，与正向移动的 file_id 一致
                // （跨卷恢复由 reconcile_file_on 的按 id 回退路径保证正确）。
                let restore_id = stable_file_id(&operation.library_id, &item.source_identity);
                database.reconcile_file(
                    &operation.library_id,
                    &restore_id,
                    &item.source_path,
                    &outcome.target_identity,
                    &outcome.target_snapshot,
                    now_ms,
                )?;
                completed += 1;
            }
            Err(error) => {
                let message = error.to_string();
                database.update_operation_status(
                    operation_id,
                    OperationStatus::RecoveryNeeded,
                    None,
                    Some(("undo_failed", &message)),
                )?;
                return Err(WorkflowError::ItemFailed {
                    ordinal: item.ordinal,
                    message,
                });
            }
        }
    }
    database.update_operation_status(
        operation_id,
        OperationStatus::RolledBack,
        Some(now_ms),
        None,
    )?;
    Ok(ExecutionReport {
        operation_id: operation_id.to_owned(),
        completed_items: completed,
        total_items: operation.items.len(),
    })
}

fn undo_copy_operation(
    database: &mut Database,
    root: &Path,
    operation: &StoredOperation,
    now_ms: i64,
) -> Result<ExecutionReport, WorkflowError> {
    let canonical_root = root.canonicalize()?;
    // 先验证全部副本，再删除任何一个，避免批量撤销产生可预见的部分结果。
    for item in operation.items.iter().rev() {
        let target = Path::new(&item.target_path);
        let canonical_target = target.canonicalize()?;
        if !contained(&canonical_target, &canonical_root) {
            return Err(WorkflowError::Move(MoveError::Verification(
                "复制目标已越过授权资料库，拒绝撤销",
            )));
        }
        let snapshot = item
            .target_snapshot
            .as_ref()
            .ok_or(WorkflowError::MissingPublishedVerification)?;
        let content_hash = item
            .content_hash
            .as_deref()
            .ok_or(WorkflowError::MissingPublishedVerification)?;
        validate_published_target(
            target,
            item.target_identity
                .as_ref()
                .ok_or(WorkflowError::MissingPublishedIdentity)?,
            snapshot,
            content_hash,
        )?;
    }

    database.update_operation_status(
        &operation.id,
        OperationStatus::RollbackPending,
        None,
        None,
    )?;
    for item in &operation.items {
        database.update_operation_item(
            &operation.id,
            item.ordinal,
            OperationStatus::RollbackPending,
            item.target_identity.as_ref(),
            None,
        )?;
    }

    let mut completed = 0;
    for item in operation.items.iter().rev() {
        let target = Path::new(&item.target_path);
        // 逐项删除前重做轻量复检（身份+快照），消除 Phase 1 全量验证与
        // 实际删除之间的 TOCTOU 窗口（H2 修复）。
        match observe_file(target) {
            Err(ObserveError::Io(ref e)) if e.kind() == io::ErrorKind::NotFound => {
                // 副本已不存在，视为该条已回滚，继续处理剩余项。
                database.update_operation_item(
                    &operation.id,
                    item.ordinal,
                    OperationStatus::RolledBack,
                    item.target_identity.as_ref(),
                    None,
                )?;
                completed += 1;
                continue;
            }
            Err(error) => {
                let message = format!("撤销前无法验证副本状态：{error}");
                database.update_operation_item(
                    &operation.id,
                    item.ordinal,
                    OperationStatus::Failed,
                    item.target_identity.as_ref(),
                    Some(("copy_undo_verify_failed", &message)),
                )?;
                database.update_operation_status(
                    &operation.id,
                    OperationStatus::RecoveryNeeded,
                    None,
                    Some(("copy_undo_verify_failed", &message)),
                )?;
                return Err(WorkflowError::ItemFailed {
                    ordinal: item.ordinal,
                    message,
                });
            }
            Ok(observed) => {
                if item
                    .target_identity
                    .as_ref()
                    .is_some_and(|identity| observed.identity != *identity)
                {
                    let message = "副本身份已变化，可能已被替换，终止撤销".to_owned();
                    database.update_operation_item(
                        &operation.id,
                        item.ordinal,
                        OperationStatus::Failed,
                        item.target_identity.as_ref(),
                        Some(("verification_changed", &message)),
                    )?;
                    database.update_operation_status(
                        &operation.id,
                        OperationStatus::RecoveryNeeded,
                        None,
                        Some(("verification_changed", &message)),
                    )?;
                    return Err(WorkflowError::ItemFailed {
                        ordinal: item.ordinal,
                        message,
                    });
                }
                if item
                    .target_snapshot
                    .as_ref()
                    .is_some_and(|snapshot| observed.snapshot != *snapshot)
                {
                    let message = "副本已被修改，终止撤销".to_owned();
                    database.update_operation_item(
                        &operation.id,
                        item.ordinal,
                        OperationStatus::Failed,
                        item.target_identity.as_ref(),
                        Some(("verification_changed", &message)),
                    )?;
                    database.update_operation_status(
                        &operation.id,
                        OperationStatus::RecoveryNeeded,
                        None,
                        Some(("verification_changed", &message)),
                    )?;
                    return Err(WorkflowError::ItemFailed {
                        ordinal: item.ordinal,
                        message,
                    });
                }
            }
        }
        if let Err(error) = fs::remove_file(target).and_then(|()| sync_parent(target)) {
            let message = error.to_string();
            database.update_operation_item(
                &operation.id,
                item.ordinal,
                OperationStatus::Failed,
                item.target_identity.as_ref(),
                Some(("copy_undo_failed", &message)),
            )?;
            database.update_operation_status(
                &operation.id,
                OperationStatus::RecoveryNeeded,
                None,
                Some(("copy_undo_failed", &message)),
            )?;
            return Err(WorkflowError::ItemFailed {
                ordinal: item.ordinal,
                message,
            });
        }
        database.mark_file_missing(&operation.library_id, &item.target_path, now_ms)?;
        database.update_operation_item(
            &operation.id,
            item.ordinal,
            OperationStatus::RolledBack,
            item.target_identity.as_ref(),
            None,
        )?;
        completed += 1;
    }
    database.update_operation_status(
        &operation.id,
        OperationStatus::RolledBack,
        Some(now_ms),
        None,
    )?;
    Ok(ExecutionReport {
        operation_id: operation.id.clone(),
        completed_items: completed,
        total_items: operation.items.len(),
    })
}

/// 启动时执行只读文件身份审计；只收敛无歧义状态，不自动继续移动或删除文件。
pub fn audit_incomplete_operations(
    database: &mut Database,
    now_ms: i64,
) -> Result<RecoveryAudit, WorkflowError> {
    let recovery = database.recovery_operations()?;
    let mut audit = RecoveryAudit::default();
    for summary in recovery {
        let operation = database
            .operation(&summary.id)?
            .ok_or(WorkflowError::OperationNotFound)?;
        let mut source_only = true;
        let mut target_only = true;
        let mut observed_targets = Vec::new();
        for item in &operation.items {
            let source = existing_identity(Path::new(&item.source_path));
            let target = existing_identity(Path::new(&item.target_path));
            source_only &= source.as_ref() == Some(&item.source_identity) && target.is_none();
            if operation.operation_kind == FileOperationKind::Copy {
                target_only &= source.as_ref() == Some(&item.source_identity)
                    && item.target_identity.is_some()
                    && target.as_ref() == item.target_identity.as_ref();
            } else {
                let expected_target = item
                    .target_identity
                    .as_ref()
                    .unwrap_or(&item.source_identity);
                target_only &= source.is_none() && target.as_ref() == Some(expected_target);
            }
            observed_targets.push(target);
        }
        if operation.status == "rollback_pending" {
            if source_only {
                for item in &operation.items {
                    database.update_operation_item(
                        &operation.id,
                        item.ordinal,
                        OperationStatus::RolledBack,
                        item.target_identity.as_ref(),
                        None,
                    )?;
                    if operation.operation_kind == FileOperationKind::Trash {
                        let observed =
                            observe_file(&item.source_path).map_err(MoveError::Observe)?;
                        database.reconcile_file(
                            &operation.library_id,
                            "restored-file",
                            &item.source_path,
                            &observed.identity,
                            &observed.snapshot,
                            now_ms,
                        )?;
                    }
                }
                database.update_operation_status(
                    &operation.id,
                    OperationStatus::RolledBack,
                    Some(now_ms),
                    None,
                )?;
                audit.completed += 1;
            } else if target_only {
                if operation.operation_kind == FileOperationKind::Trash {
                    for item in &operation.items {
                        database.mark_file_missing(
                            &operation.library_id,
                            &item.source_path,
                            now_ms,
                        )?;
                    }
                }
                database.update_operation_status(
                    &operation.id,
                    OperationStatus::Completed,
                    Some(now_ms),
                    None,
                )?;
                audit.not_started += 1;
            } else {
                database.update_operation_status(
                    &operation.id,
                    OperationStatus::RecoveryNeeded,
                    None,
                    Some((
                        "ambiguous_rollback_recovery",
                        "撤销时文件系统状态不一致，未自动修改任何文件",
                    )),
                )?;
                audit.needs_attention += 1;
            }
            continue;
        }
        if source_only {
            database.update_operation_status(
                &operation.id,
                OperationStatus::Failed,
                Some(now_ms),
                Some((
                    "recovered_not_started",
                    "操作中断于任何文件移动之前，可重新生成计划",
                )),
            )?;
            database.update_plan_status(&operation.plan_id, "ready")?;
            audit.not_started += 1;
        } else if target_only {
            for (item, target_identity) in operation.items.iter().zip(observed_targets) {
                database.update_operation_item(
                    &operation.id,
                    item.ordinal,
                    OperationStatus::Completed,
                    target_identity.as_ref(),
                    None,
                )?;
                if operation.operation_kind == FileOperationKind::Trash {
                    database.mark_file_missing(&operation.library_id, &item.source_path, now_ms)?;
                }
            }
            database.update_operation_status(
                &operation.id,
                OperationStatus::Completed,
                Some(now_ms),
                None,
            )?;
            database.update_plan_status(&operation.plan_id, "completed")?;
            audit.completed += 1;
        } else {
            database.update_operation_status(
                &operation.id,
                OperationStatus::RecoveryNeeded,
                None,
                Some((
                    "ambiguous_recovery",
                    "文件系统状态不一致，未自动修改任何文件",
                )),
            )?;
            audit.needs_attention += 1;
        }
    }
    Ok(audit)
}

/// 执行一次不覆盖已有目标的安全移动。
///
/// 同卷使用 macOS 排他原子重命名；遇到 EXDEV 时切换为“临时文件复制、
/// 哈希校验、排他发布、删除源文件”的跨卷流程。
pub fn execute_move(intent: &MoveIntent) -> Result<MoveOutcome, MoveError> {
    execute_move_inner(intent, false)
}

/// 以“临时副本、哈希校验、排他发布”流程安全复制，并保留源文件。
pub fn execute_copy(intent: &MoveIntent) -> Result<MoveOutcome, MoveError> {
    preflight(intent)?;
    let source_hash = hash_file(&intent.source)?;
    copy_publish(intent, source_hash, false)
}

fn execute_move_inner(intent: &MoveIntent, force_copy: bool) -> Result<MoveOutcome, MoveError> {
    preflight(intent)?;
    let source_hash = hash_file(&intent.source)?;

    if !force_copy {
        match rename_exclusive(&intent.source, &intent.target) {
            Ok(()) => {
                sync_parent(&intent.target)?;
                let target = observe_file(&intent.target)?;
                if target.identity != intent.expected_identity {
                    return Err(MoveError::Verification("原子移动后文件身份发生变化"));
                }
                if target.snapshot.size != intent.expected_snapshot.size
                    || hash_file(&intent.target)? != source_hash
                {
                    return Err(MoveError::Verification("原子移动后文件内容发生变化"));
                }
                return Ok(MoveOutcome {
                    transfer: TransferKind::AtomicRename,
                    target_identity: target.identity,
                    target_snapshot: target.snapshot,
                    content_hash: source_hash,
                });
            }
            Err(error)
                if error.raw_os_error() == Some(libc::EXDEV)
                    || error.raw_os_error() == Some(libc::ENOTSUP)
                    // macOS 在不支持 RENAME_EXCL 的卷（exFAT/SMB）上返回 EPERM
                    || error.raw_os_error() == Some(libc::EPERM) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(MoveError::TargetExists);
            }
            Err(error) => return Err(MoveError::Io(error)),
        }
    }

    copy_publish(intent, source_hash, true)
}

fn copy_publish(
    intent: &MoveIntent,
    source_hash: [u8; 32],
    remove_source: bool,
) -> Result<MoveOutcome, MoveError> {
    let temporary = temporary_target(&intent.target)?;
    let mut temporary_guard = TemporaryGuard::new(temporary.clone());
    copy_file_with_metadata(&intent.source, &temporary)?;
    File::options().write(true).open(&temporary)?.sync_all()?;

    // copyfile 返回后再次确认源文件仍是规划时的对象，避免复制期间被替换。
    let source_after_copy = observe_file(&intent.source)?;
    if source_after_copy.identity != intent.expected_identity
        || source_after_copy.snapshot != intent.expected_snapshot
    {
        return Err(MoveError::Verification("跨卷复制期间源文件发生变化"));
    }
    if hash_file(&temporary)? != source_hash {
        return Err(MoveError::Verification("临时副本哈希不一致"));
    }

    match rename_exclusive(&temporary, &intent.target) {
        Ok(()) => temporary_guard.disarm(),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return Err(MoveError::TargetExists);
        }
        Err(error) => return Err(MoveError::Io(error)),
    }
    sync_parent(&intent.target)?;

    let target = observe_file(&intent.target)?;
    if target.snapshot.size != intent.expected_snapshot.size
        || hash_file(&intent.target)? != source_hash
    {
        return Err(MoveError::Verification("发布后的目标文件内容不一致"));
    }
    if remove_source {
        if let Err(error) = fs::remove_file(&intent.source) {
            return Err(MoveError::PublishedSourceRetained(error));
        }
        sync_parent(&intent.source)?;
    }

    Ok(MoveOutcome {
        transfer: if remove_source {
            TransferKind::CrossVolumeCopy
        } else {
            TransferKind::VerifiedCopy
        },
        target_identity: target.identity,
        target_snapshot: target.snapshot,
        content_hash: source_hash,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryDecision {
    ResumeFromSource,
    OperationCompleted,
    RemovePublishedSource,
    ManualReview,
}

/// 根据日志里的身份快照判断崩溃后的安全恢复动作。
pub fn classify_move_recovery(
    intent: &MoveIntent,
    published_identity: Option<&FileIdentity>,
) -> RecoveryDecision {
    let source_identity = existing_identity(&intent.source);
    let target_identity = existing_identity(&intent.target);
    let source_matches = source_identity.as_ref() == Some(&intent.expected_identity);
    let target_expected = published_identity.unwrap_or(&intent.expected_identity);
    let target_matches = target_identity.as_ref() == Some(target_expected);

    match (
        source_matches,
        target_matches,
        source_identity,
        target_identity,
    ) {
        (true, false, Some(_), None) => RecoveryDecision::ResumeFromSource,
        (false, true, None, Some(_)) => RecoveryDecision::OperationCompleted,
        (true, true, Some(_), Some(_)) => RecoveryDecision::RemovePublishedSource,
        _ => RecoveryDecision::ManualReview,
    }
}

/// 撤销前必须确认目标仍是本次操作发布的对象，绝不按路径盲目回滚。
/// P1：撤销前校验——身份 + 可选快照 + 可选内容哈希（三层验证）。
pub fn validate_undo_target(
    target: &Path,
    published_identity: &FileIdentity,
    published_snapshot: Option<&FileSnapshot>,
    content_hash: Option<&str>,
) -> Result<(), MoveError> {
    let observed = observe_file(target)?;
    if observed.identity != *published_identity {
        return Err(MoveError::Verification("目标已被其他文件替换，拒绝撤销"));
    }
    if let Some(snapshot) = published_snapshot {
        if observed.snapshot != *snapshot {
            return Err(MoveError::Verification("目标在发布后已被修改，拒绝撤销"));
        }
    }
    if let Some(expected_hash) = content_hash {
        if hex_hash(hash_file(target)?) != expected_hash {
            return Err(MoveError::Verification(
                "目标内容与已发布记录不一致，拒绝撤销",
            ));
        }
    }
    Ok(())
}

fn validate_published_target(
    target: &Path,
    published_identity: &FileIdentity,
    published_snapshot: &FileSnapshot,
    published_hash: &str,
) -> Result<(), MoveError> {
    let observed = observe_file(target)?;
    if observed.identity != *published_identity {
        return Err(MoveError::Verification("目标已被其他文件替换，拒绝撤销"));
    }
    if observed.snapshot != *published_snapshot {
        return Err(MoveError::Verification("目标在复制后已被修改，拒绝撤销"));
    }
    if hex_hash(hash_file(target)?) != published_hash {
        return Err(MoveError::Verification(
            "目标内容与已发布副本不一致，拒绝撤销",
        ));
    }
    Ok(())
}

/// 从库 ID 与文件身份生成稳定记录 ID（与 guixu-indexer 的 stable_record_id 同构）。
fn stable_file_id(library_id: &str, identity: &FileIdentity) -> String {
    let mut hasher = blake3::Hasher::new();
    for component in [
        library_id,
        identity.platform.as_str(),
        &identity.volume_id,
        &identity.native_file_id,
        identity.generation.as_deref().unwrap_or(""),
    ] {
        hasher.update(component.as_bytes());
        hasher.update(&[0]);
    }
    format!("file_{}", hasher.finalize().to_hex())
}

fn existing_identity(path: &Path) -> Option<FileIdentity> {
    observe_file(path).ok().map(|observed| observed.identity)
}

fn hash_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn hex_hash(hash: [u8; 32]) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    File::open(parent)?.sync_all()
}

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temporary_target(target: &Path) -> io::Result<PathBuf> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target has no parent"))?;
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target has no file name"))?;
    for _ in 0..100 {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut candidate_name = name.to_os_string();
        candidate_name.push(format!(".guixu-{}-{sequence}.tmp", std::process::id()));
        let candidate = parent.join(candidate_name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "unable to allocate temporary target",
    ))
}

struct TemporaryGuard {
    path: PathBuf,
    armed: bool,
}

impl TemporaryGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporaryGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(target_os = "macos")]
fn rename_exclusive(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes())?;
    let target = CString::new(target.as_os_str().as_bytes())?;
    // SAFETY: C strings live for the duration of the call and contain no interior NUL.
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
fn rename_exclusive(source: &Path, target: &Path) -> io::Result<()> {
    fs::hard_link(source, target)?;
    fs::remove_file(source)
}

#[cfg(target_os = "macos")]
fn copy_file_with_metadata(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes())?;
    let target = CString::new(target.as_os_str().as_bytes())?;
    let flags = libc::COPYFILE_DATA
        | libc::COPYFILE_METADATA
        | libc::COPYFILE_EXCL
        | libc::COPYFILE_NOFOLLOW;
    // SAFETY: the paths are valid C strings and a null copyfile state requests defaults.
    let result = unsafe {
        libc::copyfile(
            source.as_ptr(),
            target.as_ptr(),
            std::ptr::null_mut(),
            flags,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
fn copy_file_with_metadata(source: &Path, target: &Path) -> io::Result<()> {
    fs::copy(source, target)?;
    fs::set_permissions(target, fs::metadata(source)?.permissions())
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("不允许从 {from:?} 转换到 {to:?}")]
pub struct InvalidTransition {
    pub from: OperationStatus,
    pub to: OperationStatus,
}

pub fn transition(
    current: OperationStatus,
    next: OperationStatus,
) -> Result<OperationStatus, InvalidTransition> {
    use OperationStatus::*;
    let valid = matches!(
        (current, next),
        (Draft, Planned)
            | (Planned, PreflightPassed | Conflict | Cancelled)
            | (PreflightPassed, IntentLogged | Conflict | Cancelled)
            | (IntentLogged, Executing | RecoveryNeeded)
            | (Executing, Published | Failed | RecoveryNeeded)
            | (Published, Verified | RecoveryNeeded)
            | (Verified, Completed | RollbackPending)
            | (Completed, RollbackPending)
            | (RollbackPending, RolledBack | RecoveryNeeded | Failed)
            | (RecoveryNeeded, Executing | RollbackPending | Failed)
    );
    valid.then_some(next).ok_or(InvalidTransition {
        from: current,
        to: next,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use guixu_domain::ConflictPolicy;
    use guixu_storage::{NewPlan, NewPlanItem};

    use super::*;

    fn intent(directory: &Path) -> MoveIntent {
        let source = directory.join("source.txt");
        let target_dir = directory.join("target");
        fs::create_dir_all(&target_dir).expect("target directory");
        fs::write(&source, b"original").expect("source fixture");
        let observed = observe_file(&source).expect("observe source");
        MoveIntent {
            root: directory.to_owned(),
            source,
            target: target_dir.join("source.txt"),
            expected_identity: observed.identity,
            expected_snapshot: observed.snapshot,
        }
    }

    fn rename_request(path: &Path, file_id: &str, new_name: &str) -> RenameRequest {
        let observed = observe_file(path).expect("observe rename fixture");
        RenameRequest {
            file_id: file_id.to_owned(),
            source: path.to_owned(),
            new_name: new_name.to_owned(),
            expected_identity: observed.identity,
            expected_snapshot: observed.snapshot,
        }
    }

    #[test]
    fn rename_planner_is_read_only_and_returns_stable_targets() {
        let directory = tempfile::tempdir().expect("library directory");
        let first = directory.path().join("乙.txt");
        let second = directory.path().join("甲.txt");
        fs::write(&first, b"first").expect("first fixture");
        fs::write(&second, b"second").expect("second fixture");
        let planned = plan_renames(
            directory.path(),
            &[
                rename_request(&first, "file-b", "新乙.txt"),
                rename_request(&second, "file-a", "新甲.txt"),
            ],
        )
        .expect("rename plan");
        let canonical_directory = directory.path().canonicalize().expect("canonical library");
        let mut expected_sources = vec![
            first.canonicalize().expect("first path"),
            second.canonicalize().expect("second path"),
        ];
        expected_sources.sort();
        assert_eq!(
            planned
                .iter()
                .map(|item| item.source.clone())
                .collect::<Vec<_>>(),
            expected_sources
        );
        assert_eq!(
            planned
                .iter()
                .find(|item| item.file_id == "file-a")
                .expect("first planned item")
                .target,
            canonical_directory.join("新甲.txt")
        );
        assert_eq!(
            planned
                .iter()
                .find(|item| item.file_id == "file-b")
                .expect("second planned item")
                .target,
            canonical_directory.join("新乙.txt")
        );
        assert!(first.exists());
        assert!(second.exists());
        assert!(!directory.path().join("新甲.txt").exists());
    }

    #[test]
    fn rename_planner_rejects_invalid_and_reserved_names() {
        let directory = tempfile::tempdir().expect("library directory");
        let source = directory.path().join("source.txt");
        fs::write(&source, b"source").expect("fixture");
        for invalid in ["", "../escape.txt", "nested/file.txt", "CON.txt", "tail."] {
            assert!(matches!(
                plan_renames(
                    directory.path(),
                    &[rename_request(&source, "file", invalid)]
                ),
                Err(RenamePlanError::InvalidName { .. })
            ));
        }
    }

    #[test]
    fn rename_planner_rejects_existing_and_duplicate_targets() {
        let directory = tempfile::tempdir().expect("library directory");
        let first = directory.path().join("first.txt");
        let second = directory.path().join("second.txt");
        let occupied = directory.path().join("occupied.txt");
        fs::write(&first, b"first").expect("first fixture");
        fs::write(&second, b"second").expect("second fixture");
        fs::write(&occupied, b"occupied").expect("occupied fixture");
        assert!(matches!(
            plan_renames(
                directory.path(),
                &[rename_request(&first, "first", "occupied.txt")]
            ),
            Err(RenamePlanError::Preflight {
                source: PreflightError::TargetExists,
                ..
            })
        ));
        assert!(matches!(
            plan_renames(
                directory.path(),
                &[
                    rename_request(&first, "first", "same.txt"),
                    rename_request(&second, "second", "SAME.txt"),
                ]
            ),
            Err(RenamePlanError::DuplicateTarget { .. })
        ));
    }

    #[test]
    fn copy_planner_is_read_only_and_rejects_targets_outside_the_library() {
        let directory = tempfile::tempdir().expect("library directory");
        let source = directory.path().join("source.txt");
        let destination = directory.path().join("copies");
        fs::create_dir(&destination).expect("copy destination");
        fs::write(&source, b"copy me").expect("source fixture");
        let observed = observe_file(&source).expect("observe source");
        let request = CopyRequest {
            file_id: "file-1".to_owned(),
            source: source.clone(),
            expected_identity: observed.identity,
            expected_snapshot: observed.snapshot,
        };
        let planned = plan_copies(
            directory.path(),
            &destination,
            std::slice::from_ref(&request),
        )
        .expect("copy plan");
        assert_eq!(
            planned[0].target,
            destination
                .canonicalize()
                .expect("canonical destination")
                .join("source.txt")
        );
        assert!(source.exists());
        assert!(!planned[0].target.exists());

        let outside = tempfile::tempdir().expect("outside directory");
        assert!(matches!(
            plan_copies(directory.path(), outside.path(), &[request]),
            Err(CopyPlanError::InvalidDestination)
        ));
    }

    #[test]
    fn trash_planner_preserves_relative_paths_inside_an_isolated_namespace() {
        let directory = tempfile::tempdir().expect("library directory");
        let nested = directory.path().join("documents/reports");
        fs::create_dir_all(&nested).expect("nested directory");
        let source = nested.join("annual.txt");
        fs::write(&source, b"annual report").expect("source fixture");
        let observed = observe_file(&source).expect("observe source");
        let planned = plan_trash(
            directory.path(),
            "trash-plan-1",
            &[CopyRequest {
                file_id: "file-1".to_owned(),
                source: source.clone(),
                expected_identity: observed.identity,
                expected_snapshot: observed.snapshot,
            }],
        )
        .expect("trash plan");
        assert_eq!(
            planned[0].target,
            directory
                .path()
                .canonicalize()
                .expect("canonical root")
                .join(".guixu-trash/trash-plan-1/documents/reports/annual.txt")
        );
        assert!(source.exists());
        assert!(!planned[0].target.exists());
    }

    #[test]
    fn rename_planner_rejects_a_source_changed_after_selection() {
        let directory = tempfile::tempdir().expect("library directory");
        let source = directory.path().join("source.txt");
        fs::write(&source, b"source").expect("fixture");
        let request = rename_request(&source, "file", "renamed.txt");
        fs::write(&source, b"changed and longer").expect("change source");
        assert!(matches!(
            plan_renames(directory.path(), &[request]),
            Err(RenamePlanError::Preflight {
                source: PreflightError::SnapshotChanged,
                ..
            })
        ));
    }

    #[test]
    fn preflight_accepts_unchanged_source_and_free_target() {
        let directory = tempfile::tempdir().expect("temp directory");
        assert!(preflight(&intent(directory.path())).is_ok());
    }

    #[test]
    fn preflight_rejects_changed_source() {
        let directory = tempfile::tempdir().expect("temp directory");
        let intent = intent(directory.path());
        fs::write(&intent.source, b"changed and longer").expect("change source");
        assert!(matches!(
            preflight(&intent),
            Err(PreflightError::SnapshotChanged)
        ));
    }

    #[test]
    fn preflight_never_accepts_an_existing_target() {
        let directory = tempfile::tempdir().expect("temp directory");
        let intent = intent(directory.path());
        fs::write(&intent.target, b"do not overwrite").expect("target fixture");
        assert!(matches!(
            preflight(&intent),
            Err(PreflightError::TargetExists)
        ));
    }

    #[test]
    fn preflight_accepts_a_safe_target_with_missing_parent_directories() {
        let directory = tempfile::tempdir().expect("temp directory");
        let mut intent = intent(directory.path());
        intent.target = directory.path().join("new/category/source.txt");
        assert!(preflight(&intent).is_ok());
        assert!(!directory.path().join("new").exists());
    }

    #[test]
    fn preflight_rejects_a_missing_target_tree_outside_the_root() {
        let directory = tempfile::tempdir().expect("temp directory");
        let outside = tempfile::tempdir().expect("outside directory");
        let mut intent = intent(directory.path());
        intent.target = outside.path().join("new/category/source.txt");
        assert!(matches!(
            preflight(&intent),
            Err(PreflightError::OutsideRoot)
        ));
    }

    #[test]
    fn operation_state_machine_rejects_skipped_durability_steps() {
        assert_eq!(
            transition(OperationStatus::Draft, OperationStatus::Executing),
            Err(InvalidTransition {
                from: OperationStatus::Draft,
                to: OperationStatus::Executing
            })
        );
        assert_eq!(
            transition(OperationStatus::Draft, OperationStatus::Planned),
            Ok(OperationStatus::Planned)
        );
    }

    #[test]
    fn atomic_move_preserves_identity_and_content() {
        let directory = tempfile::tempdir().expect("temp directory");
        let intent = intent(directory.path());
        let outcome = execute_move(&intent).expect("safe move");
        assert_eq!(outcome.transfer, TransferKind::AtomicRename);
        assert_eq!(outcome.target_identity, intent.expected_identity);
        assert!(!intent.source.exists());
        assert_eq!(fs::read(&intent.target).expect("read target"), b"original");
    }

    #[test]
    fn copy_publish_path_verifies_then_removes_source() {
        let directory = tempfile::tempdir().expect("temp directory");
        let intent = intent(directory.path());
        let outcome = execute_move_inner(&intent, true).expect("copy move");
        assert_eq!(outcome.transfer, TransferKind::CrossVolumeCopy);
        assert!(!intent.source.exists());
        assert_eq!(fs::read(&intent.target).expect("read target"), b"original");
    }

    #[test]
    fn existing_target_is_never_overwritten_during_execution() {
        let directory = tempfile::tempdir().expect("temp directory");
        let intent = intent(directory.path());
        fs::write(&intent.target, b"keep me").expect("target fixture");
        assert!(matches!(
            execute_move(&intent),
            Err(MoveError::Preflight(_))
        ));
        assert_eq!(fs::read(&intent.target).expect("read target"), b"keep me");
    }

    #[test]
    fn recovery_distinguishes_resume_complete_and_ambiguous_states() {
        let directory = tempfile::tempdir().expect("temp directory");
        let intent = intent(directory.path());
        assert_eq!(
            classify_move_recovery(&intent, None),
            RecoveryDecision::ResumeFromSource
        );
        fs::rename(&intent.source, &intent.target).expect("publish fixture");
        assert_eq!(
            classify_move_recovery(&intent, None),
            RecoveryDecision::OperationCompleted
        );
        fs::write(&intent.source, b"replacement").expect("replacement fixture");
        assert_eq!(
            classify_move_recovery(&intent, None),
            RecoveryDecision::ManualReview
        );
    }

    #[test]
    fn undo_rejects_a_replaced_target() {
        let directory = tempfile::tempdir().expect("temp directory");
        let intent = intent(directory.path());
        let outcome = execute_move(&intent).expect("safe move");
        fs::remove_file(&intent.target).expect("remove published target");
        fs::write(&intent.target, b"different object").expect("replace target");
        assert!(matches!(
            validate_undo_target(
                &intent.target,
                &outcome.target_identity,
                Some(&outcome.target_snapshot),
                None
            ),
            Err(MoveError::Verification(_))
        ));
    }

    fn persisted_plan(directory: &Path) -> (Database, PathBuf, PathBuf) {
        let root = directory.join("library");
        fs::create_dir_all(&root).expect("library root");
        let source = root.join("report.txt");
        let target = root.join("归序整理/文档/report.txt");
        fs::write(&source, b"important report").expect("source fixture");
        let observed = observe_file(&source).expect("observe fixture");
        let mut database = Database::open(directory.join("workflow.sqlite3")).expect("database");
        database
            .connection()
            .execute(
                "INSERT INTO libraries(id,name,created_at_ms,updated_at_ms) VALUES('library','测试',1,1)",
                [],
            )
            .expect("library fixture");
        database
            .reconcile_file(
                "library",
                "file-1",
                source.to_str().expect("source text"),
                &observed.identity,
                &observed.snapshot,
                5,
            )
            .expect("index fixture");
        database
            .create_plan(NewPlan {
                id: "plan-1",
                library_id: "library",
                operation_kind: FileOperationKind::Organize,
                conflict_policy: ConflictPolicy::Abort,
                created_at_ms: 10,
                expires_at_ms: 1_000,
                items: &[NewPlanItem {
                    ordinal: 0,
                    file_id: "file-1".to_owned(),
                    source_path: source.to_string_lossy().into_owned(),
                    target_path: target.to_string_lossy().into_owned(),
                    expected_identity: observed.identity,
                    expected_snapshot: observed.snapshot,
                }],
            })
            .expect("plan fixture");
        (database, source, target)
    }

    #[test]
    fn persisted_workflow_executes_and_undoes_without_data_loss() {
        let directory = tempfile::tempdir().expect("temp directory");
        let (mut database, source, target) = persisted_plan(directory.path());
        let executed = execute_stored_plan(
            &mut database,
            source.parent().expect("root"),
            "plan-1",
            "operation-1",
            20,
        )
        .expect("execute plan");
        assert_eq!(executed.completed_items, 1);
        assert!(!source.exists());
        assert_eq!(
            fs::read(&target).expect("target content"),
            b"important report"
        );
        assert_eq!(
            database
                .operation("operation-1")
                .expect("history")
                .expect("operation")
                .status,
            "completed"
        );

        let undone = undo_stored_operation(
            &mut database,
            source.parent().expect("root"),
            "operation-1",
            30,
        )
        .expect("undo operation");
        assert_eq!(undone.completed_items, 1);
        assert!(!target.exists());
        assert_eq!(
            fs::read(&source).expect("restored content"),
            b"important report"
        );
        assert_eq!(
            database
                .operation("operation-1")
                .expect("history")
                .expect("operation")
                .status,
            "rolled_back"
        );
    }

    #[test]
    fn changed_plan_source_fails_before_logging_or_moving_any_item() {
        let directory = tempfile::tempdir().expect("temp directory");
        let (mut database, source, target) = persisted_plan(directory.path());
        fs::write(&source, b"changed after preview and much longer").expect("change source");
        assert!(matches!(
            execute_stored_plan(
                &mut database,
                source.parent().expect("root"),
                "plan-1",
                "operation-1",
                20,
            ),
            Err(WorkflowError::Preflight(PreflightError::SnapshotChanged))
        ));
        assert!(source.exists());
        assert!(!target.exists());
        assert!(
            database
                .operation_history("library", 20)
                .expect("history")
                .is_empty()
        );
    }

    #[test]
    fn rename_plan_executes_updates_the_index_and_undoes() {
        let directory = tempfile::tempdir().expect("temp directory");
        let root = directory.path().join("library");
        fs::create_dir_all(&root).expect("library root");
        let source = root.join("before.txt");
        let target = root.join("after.txt");
        fs::write(&source, b"content").expect("source fixture");
        let observed = observe_file(&source).expect("observe source");
        let mut database = Database::open(directory.path().join("rename.sqlite3")).expect("db");
        database
            .connection()
            .execute(
                "INSERT INTO libraries(id,name,created_at_ms,updated_at_ms)
                 VALUES('library','测试',1,1)",
                [],
            )
            .expect("library fixture");
        database
            .reconcile_file(
                "library",
                "file-1",
                source.to_str().expect("source path"),
                &observed.identity,
                &observed.snapshot,
                5,
            )
            .expect("index fixture");
        database
            .create_plan(NewPlan {
                id: "rename-plan",
                library_id: "library",
                operation_kind: FileOperationKind::Rename,
                conflict_policy: ConflictPolicy::Abort,
                created_at_ms: 10,
                expires_at_ms: 1_000,
                items: &[NewPlanItem {
                    ordinal: 0,
                    file_id: "file-1".to_owned(),
                    source_path: source.to_string_lossy().into_owned(),
                    target_path: target.to_string_lossy().into_owned(),
                    expected_identity: observed.identity,
                    expected_snapshot: observed.snapshot,
                }],
            })
            .expect("rename plan");
        let report = execute_stored_plan(&mut database, &root, "rename-plan", "operation", 20)
            .expect("execute rename");
        assert_eq!(report.completed_items, 1);
        assert!(!source.exists());
        assert_eq!(fs::read(&target).expect("renamed content"), b"content");
        assert_eq!(
            database
                .indexed_file("library", "file-1")
                .expect("indexed file")
                .expect("file record")
                .current_path,
            target.to_string_lossy()
        );
        assert_eq!(
            database
                .operation("operation")
                .expect("operation")
                .expect("history")
                .operation_kind,
            FileOperationKind::Rename
        );
        undo_stored_operation(&mut database, &root, "operation", 30).expect("undo rename");
        assert!(source.exists());
        assert!(!target.exists());
    }

    #[test]
    fn injected_mid_batch_conflict_enters_recovery_without_touching_later_source() {
        let directory = tempfile::tempdir().expect("temp directory");
        let root = directory.path().join("library");
        fs::create_dir_all(&root).expect("library root");
        let sources = [root.join("first.txt"), root.join("second.txt")];
        fs::write(&sources[0], b"first payload").expect("first source");
        fs::write(&sources[1], b"second payload").expect("second source");
        let targets = [
            root.join("sorted/first.txt"),
            root.join("sorted/second.txt"),
        ];
        let observed = sources
            .iter()
            .map(|source| observe_file(source).expect("observe source"))
            .collect::<Vec<_>>();
        let mut database =
            Database::open(directory.path().join("fault.sqlite3")).expect("database");
        database
            .connection()
            .execute(
                "INSERT INTO libraries(id,name,created_at_ms,updated_at_ms) VALUES('library','测试',1,1)",
                [],
            )
            .expect("library fixture");
        for index in 0..2 {
            database
                .reconcile_file(
                    "library",
                    &format!("file-{index}"),
                    sources[index].to_str().expect("source path"),
                    &observed[index].identity,
                    &observed[index].snapshot,
                    5,
                )
                .expect("index fixture");
        }
        let items = (0..2)
            .map(|index| NewPlanItem {
                ordinal: index as i64,
                file_id: format!("file-{index}"),
                source_path: sources[index].to_string_lossy().into_owned(),
                target_path: targets[index].to_string_lossy().into_owned(),
                expected_identity: observed[index].identity.clone(),
                expected_snapshot: observed[index].snapshot.clone(),
            })
            .collect::<Vec<_>>();
        database
            .create_plan(NewPlan {
                id: "fault-plan",
                library_id: "library",
                operation_kind: FileOperationKind::Organize,
                conflict_policy: ConflictPolicy::Abort,
                created_at_ms: 10,
                expires_at_ms: 1_000,
                items: &items,
            })
            .expect("plan fixture");

        let result = execute_stored_plan_with_hook(
            &mut database,
            &root,
            "fault-plan",
            "fault-operation",
            20,
            |ordinal, intent| {
                if ordinal == 1 {
                    fs::create_dir_all(intent.target.parent().expect("target parent"))?;
                    fs::write(&intent.target, b"external file appeared")?;
                }
                Ok(())
            },
        );

        assert!(matches!(
            result,
            Err(WorkflowError::ItemFailed { ordinal: 1, .. })
        ));
        assert!(targets[0].exists());
        assert!(!sources[0].exists());
        assert!(sources[1].exists());
        assert_eq!(
            fs::read(&targets[1]).expect("conflict target"),
            b"external file appeared"
        );
        assert_eq!(
            database
                .operation("fault-operation")
                .expect("operation query")
                .expect("operation")
                .status,
            "recovery_needed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn injected_symlinked_target_directory_cannot_escape_authorized_root() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temp directory");
        let root = directory.path().join("library");
        let outside = directory.path().join("outside");
        fs::create_dir_all(&root).expect("library root");
        fs::create_dir_all(&outside).expect("outside root");
        let sources = [root.join("first.txt"), root.join("second.txt")];
        fs::write(&sources[0], b"first payload").expect("first source");
        fs::write(&sources[1], b"second payload").expect("second source");
        let targets = [
            root.join("safe/first.txt"),
            root.join("redirected/second.txt"),
        ];
        let observed = sources
            .iter()
            .map(|source| observe_file(source).expect("observe source"))
            .collect::<Vec<_>>();
        let mut database =
            Database::open(directory.path().join("symlink-fault.sqlite3")).expect("database");
        database
            .connection()
            .execute(
                "INSERT INTO libraries(id,name,created_at_ms,updated_at_ms) VALUES('library','测试',1,1)",
                [],
            )
            .expect("library fixture");
        for index in 0..2 {
            database
                .reconcile_file(
                    "library",
                    &format!("file-{index}"),
                    sources[index].to_str().expect("source path"),
                    &observed[index].identity,
                    &observed[index].snapshot,
                    5,
                )
                .expect("index fixture");
        }
        let items = (0..2)
            .map(|index| NewPlanItem {
                ordinal: index as i64,
                file_id: format!("file-{index}"),
                source_path: sources[index].to_string_lossy().into_owned(),
                target_path: targets[index].to_string_lossy().into_owned(),
                expected_identity: observed[index].identity.clone(),
                expected_snapshot: observed[index].snapshot.clone(),
            })
            .collect::<Vec<_>>();
        database
            .create_plan(NewPlan {
                id: "symlink-fault-plan",
                library_id: "library",
                operation_kind: FileOperationKind::Organize,
                conflict_policy: ConflictPolicy::Abort,
                created_at_ms: 10,
                expires_at_ms: 1_000,
                items: &items,
            })
            .expect("plan fixture");

        let result = execute_stored_plan_with_hook(
            &mut database,
            &root,
            "symlink-fault-plan",
            "symlink-fault-operation",
            20,
            |ordinal, intent| {
                if ordinal == 1 {
                    symlink(&outside, intent.target.parent().expect("target parent"))?;
                }
                Ok(())
            },
        );

        assert!(matches!(
            result,
            Err(WorkflowError::ItemFailed { ordinal: 1, .. })
        ));
        assert!(targets[0].exists());
        assert!(!sources[0].exists());
        assert!(sources[1].exists());
        assert!(!outside.join("second.txt").exists());
        assert_eq!(
            database
                .operation("symlink-fault-operation")
                .expect("operation query")
                .expect("operation")
                .status,
            "recovery_needed"
        );
    }

    #[test]
    fn copy_plan_retains_source_and_undo_removes_only_the_verified_copy() {
        let directory = tempfile::tempdir().expect("temp directory");
        let root = directory.path().join("library");
        let copies = root.join("copies");
        fs::create_dir_all(&copies).expect("copy directory");
        let source = root.join("source.txt");
        let target = copies.join("source.txt");
        fs::write(&source, b"valuable content").expect("source fixture");
        let observed = observe_file(&source).expect("observe source");
        let mut database = Database::open(directory.path().join("copy.sqlite3")).expect("db");
        database
            .connection()
            .execute(
                "INSERT INTO libraries(id,name,created_at_ms,updated_at_ms)
                 VALUES('library','测试',1,1)",
                [],
            )
            .expect("library fixture");
        database
            .reconcile_file(
                "library",
                "file-1",
                source.to_str().expect("source path"),
                &observed.identity,
                &observed.snapshot,
                5,
            )
            .expect("index fixture");
        database
            .create_plan(NewPlan {
                id: "copy-plan",
                library_id: "library",
                operation_kind: FileOperationKind::Copy,
                conflict_policy: ConflictPolicy::Abort,
                created_at_ms: 10,
                expires_at_ms: 1_000,
                items: &[NewPlanItem {
                    ordinal: 0,
                    file_id: "file-1".to_owned(),
                    source_path: source.to_string_lossy().into_owned(),
                    target_path: target.to_string_lossy().into_owned(),
                    expected_identity: observed.identity,
                    expected_snapshot: observed.snapshot,
                }],
            })
            .expect("copy plan");

        execute_stored_plan(&mut database, &root, "copy-plan", "copy-operation", 20)
            .expect("execute copy");
        assert_eq!(
            fs::read(&source).expect("source remains"),
            b"valuable content"
        );
        assert_eq!(
            fs::read(&target).expect("copy content"),
            b"valuable content"
        );
        let operation = database
            .operation("copy-operation")
            .expect("operation query")
            .expect("operation");
        assert!(operation.items[0].target_snapshot.is_some());
        assert!(operation.items[0].content_hash.is_some());

        undo_stored_operation(&mut database, &root, "copy-operation", 30).expect("undo copy");
        assert!(source.exists());
        assert!(!target.exists());
        assert_eq!(
            database
                .operation("copy-operation")
                .expect("operation query")
                .expect("operation")
                .status,
            "rolled_back"
        );
    }

    #[test]
    fn copy_undo_refuses_to_delete_a_modified_copy() {
        let directory = tempfile::tempdir().expect("temp directory");
        let copy_intent = intent(directory.path());
        let outcome = execute_copy(&copy_intent).expect("safe copy");
        assert_eq!(outcome.transfer, TransferKind::VerifiedCopy);
        assert!(copy_intent.source.exists());
        fs::write(&copy_intent.target, b"edited after copy").expect("modify copy");
        assert!(
            validate_published_target(
                &copy_intent.target,
                &outcome.target_identity,
                &outcome.target_snapshot,
                &hex_hash(outcome.content_hash),
            )
            .is_err()
        );
        assert!(copy_intent.target.exists());
    }

    #[test]
    fn recovery_marks_an_unambiguously_finished_copy_rollback_as_rolled_back() {
        let directory = tempfile::tempdir().expect("temp directory");
        let (mut database, source, target) = persisted_plan(directory.path());
        database
            .connection()
            .execute(
                "UPDATE plans SET operation_kind='copy' WHERE id='plan-1'",
                [],
            )
            .expect("copy plan kind");
        execute_stored_plan(
            &mut database,
            source.parent().expect("root"),
            "plan-1",
            "copy-operation",
            20,
        )
        .expect("execute copy");
        database
            .update_operation_status(
                "copy-operation",
                OperationStatus::RollbackPending,
                None,
                None,
            )
            .expect("rollback pending");
        fs::remove_file(&target).expect("simulate completed delete before journal update");

        let audit = audit_incomplete_operations(&mut database, 30).expect("recovery audit");
        assert_eq!(audit.completed, 1);
        assert_eq!(
            database
                .operation("copy-operation")
                .expect("operation query")
                .expect("operation")
                .status,
            "rolled_back"
        );
        assert!(source.exists());
    }

    #[test]
    fn trash_operation_hides_the_indexed_file_and_undo_restores_it() {
        let directory = tempfile::tempdir().expect("temp directory");
        let (mut database, source, target) = persisted_plan(directory.path());
        database
            .connection()
            .execute(
                "UPDATE plans SET operation_kind='trash' WHERE id='plan-1'",
                [],
            )
            .expect("trash plan kind");
        execute_stored_plan(
            &mut database,
            source.parent().expect("root"),
            "plan-1",
            "trash-operation",
            20,
        )
        .expect("execute trash");
        assert!(!source.exists());
        assert!(target.exists());
        assert!(
            database
                .list_files_page("library", None, 10)
                .expect("active files")
                .items
                .is_empty()
        );

        undo_stored_operation(
            &mut database,
            source.parent().expect("root"),
            "trash-operation",
            30,
        )
        .expect("restore trash");
        assert!(source.exists());
        assert!(!target.exists());
        assert_eq!(
            database
                .list_files_page("library", None, 10)
                .expect("restored files")
                .items
                .len(),
            1
        );
    }

    #[test]
    fn recovery_audit_converges_only_unambiguous_filesystem_states() {
        let untouched_directory = tempfile::tempdir().expect("untouched directory");
        let (mut untouched_db, untouched_source, _) = persisted_plan(untouched_directory.path());
        let untouched_plan = untouched_db
            .plan("plan-1")
            .expect("plan")
            .expect("stored plan");
        untouched_db
            .log_operation_intent(
                "operation-untouched",
                "plan-1",
                20,
                &[OperationItemIntent {
                    ordinal: 0,
                    source_path: untouched_plan.items[0].source_path.clone(),
                    target_path: untouched_plan.items[0].target_path.clone(),
                    source_identity: untouched_plan.items[0].expected_identity.clone(),
                }],
            )
            .expect("intent");
        let untouched = audit_incomplete_operations(&mut untouched_db, 30).expect("audit");
        assert_eq!(untouched.not_started, 1);
        assert!(untouched_source.exists());
        assert_eq!(
            untouched_db
                .operation("operation-untouched")
                .expect("operation")
                .expect("history")
                .status,
            "failed"
        );

        let moved_directory = tempfile::tempdir().expect("moved directory");
        let (mut moved_db, moved_source, moved_target) = persisted_plan(moved_directory.path());
        let moved_plan = moved_db.plan("plan-1").expect("plan").expect("stored plan");
        moved_db
            .log_operation_intent(
                "operation-moved",
                "plan-1",
                20,
                &[OperationItemIntent {
                    ordinal: 0,
                    source_path: moved_plan.items[0].source_path.clone(),
                    target_path: moved_plan.items[0].target_path.clone(),
                    source_identity: moved_plan.items[0].expected_identity.clone(),
                }],
            )
            .expect("intent");
        fs::create_dir_all(moved_target.parent().expect("target parent")).expect("target parent");
        fs::rename(&moved_source, &moved_target).expect("simulate published move");
        let moved = audit_incomplete_operations(&mut moved_db, 30).expect("audit");
        assert_eq!(moved.completed, 1);
        assert_eq!(
            moved_db
                .operation("operation-moved")
                .expect("operation")
                .expect("history")
                .status,
            "completed"
        );
    }

    #[test]
    fn recovery_hides_a_trash_item_published_before_the_index_update() {
        let directory = tempfile::tempdir().expect("temp directory");
        let (mut database, source, target) = persisted_plan(directory.path());
        database
            .connection()
            .execute(
                "UPDATE plans SET operation_kind='trash' WHERE id='plan-1'",
                [],
            )
            .expect("trash plan kind");
        let plan = database.plan("plan-1").expect("plan").expect("stored plan");
        database
            .log_operation_intent(
                "trash-interrupted",
                "plan-1",
                20,
                &[OperationItemIntent {
                    ordinal: 0,
                    source_path: plan.items[0].source_path.clone(),
                    target_path: plan.items[0].target_path.clone(),
                    source_identity: plan.items[0].expected_identity.clone(),
                }],
            )
            .expect("intent");
        fs::create_dir_all(target.parent().expect("target parent")).expect("target parent");
        fs::rename(&source, &target).expect("simulate published trash move");

        let audit = audit_incomplete_operations(&mut database, 30).expect("audit");
        assert_eq!(audit.completed, 1);
        assert!(
            database
                .list_files_page("library", None, 10)
                .expect("active files")
                .items
                .is_empty()
        );
    }
}
