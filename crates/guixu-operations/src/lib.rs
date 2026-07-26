use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use guixu_domain::{FileIdentity, FileSnapshot, OperationStatus};
use guixu_platform::{ObserveError, observe_file};
use guixu_storage::{Database, OperationItemIntent, StorageError};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveIntent {
    pub root: PathBuf,
    pub source: PathBuf,
    pub target: PathBuf,
    pub expected_identity: FileIdentity,
    pub expected_snapshot: FileSnapshot,
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
        return Err(PreflightError::TargetExists);
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
    #[error("操作记录不存在")]
    OperationNotFound,
    #[error("当前操作状态不可撤销：{0}")]
    OperationNotUndoable(String),
    #[error("操作条目缺少已发布文件身份")]
    MissingPublishedIdentity,
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

    let mut completed = 0;
    for (item, intent) in plan.items.iter().zip(&intents) {
        let result = (|| -> Result<MoveOutcome, MoveError> {
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
            execute_move(intent)
        })();
        match result {
            Ok(outcome) => {
                database.update_operation_item(
                    operation_id,
                    item.ordinal,
                    OperationStatus::Completed,
                    Some(&outcome.target_identity),
                    None,
                )?;
                database.reconcile_file(
                    &plan.library_id,
                    item.file_id.as_deref().unwrap_or("moved-file"),
                    &item.target_path,
                    &outcome.target_identity,
                    &outcome.target_snapshot,
                    now_ms,
                )?;
                completed += 1;
            }
            Err(error) => {
                let message = error.to_string();
                database.update_operation_item(
                    operation_id,
                    item.ordinal,
                    OperationStatus::Failed,
                    None,
                    Some(("move_failed", &message)),
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
                    Some(("move_failed", &message)),
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
    if operation.status != "completed" {
        return Err(WorkflowError::OperationNotUndoable(operation.status));
    }
    let mut reverse = Vec::with_capacity(operation.items.len());
    for item in operation.items.iter().rev() {
        let published_identity = item
            .target_identity
            .as_ref()
            .ok_or(WorkflowError::MissingPublishedIdentity)?;
        validate_undo_target(Path::new(&item.target_path), published_identity)?;
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
                database.reconcile_file(
                    &operation.library_id,
                    "restored-file",
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
            let expected_target = item
                .target_identity
                .as_ref()
                .unwrap_or(&item.source_identity);
            target_only &= source.is_none() && target.as_ref() == Some(expected_target);
            observed_targets.push(target);
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
            Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(MoveError::TargetExists);
            }
            Err(error) => return Err(MoveError::Io(error)),
        }
    }

    copy_publish_remove(intent, source_hash)
}

fn copy_publish_remove(
    intent: &MoveIntent,
    source_hash: [u8; 32],
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
    if let Err(error) = fs::remove_file(&intent.source) {
        return Err(MoveError::PublishedSourceRetained(error));
    }
    sync_parent(&intent.source)?;

    Ok(MoveOutcome {
        transfer: TransferKind::CrossVolumeCopy,
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
pub fn validate_undo_target(
    target: &Path,
    published_identity: &FileIdentity,
) -> Result<(), MoveError> {
    let observed = observe_file(target)?;
    if observed.identity != *published_identity {
        return Err(MoveError::Verification("目标已被其他文件替换，拒绝撤销"));
    }
    Ok(())
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

    use guixu_storage::NewPlanItem;

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
            validate_undo_target(&intent.target, &outcome.target_identity),
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
            .create_plan(
                "plan-1",
                "library",
                10,
                1_000,
                &[NewPlanItem {
                    ordinal: 0,
                    file_id: "file-1".to_owned(),
                    source_path: source.to_string_lossy().into_owned(),
                    target_path: target.to_string_lossy().into_owned(),
                    expected_identity: observed.identity,
                    expected_snapshot: observed.snapshot,
                }],
            )
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
}
