use serde::{Deserialize, Serialize};

use crate::{FileRecordId, JobId, LibraryId, OperationId, PlanId, RootId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    MacOs,
    Windows,
    Linux,
    Other,
}

impl Platform {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MacOs => "macos",
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileIdentity {
    pub platform: Platform,
    pub volume_id: String,
    pub native_file_id: String,
    pub generation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Library {
    pub id: LibraryId,
    pub name: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryRoot {
    pub id: RootId,
    pub library_id: LibraryId,
    pub path: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub size: u64,
    pub modified_at_ns: i64,
    pub changed_at_ns: i64,
    pub created_at_ns: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: FileRecordId,
    pub library_id: LibraryId,
    pub identity: FileIdentity,
    pub current_path: String,
    pub snapshot: FileSnapshot,
}

/// 用户发起的文件操作类型。计划、执行记录和 UI 必须共享同一语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOperationKind {
    Organize,
    Rename,
    Move,
    Copy,
    Trash,
}

impl FileOperationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Organize => "organize",
            Self::Rename => "rename",
            Self::Move => "move",
            Self::Copy => "copy",
            Self::Trash => "trash",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "organize" => Some(Self::Organize),
            "rename" => Some(Self::Rename),
            "move" => Some(Self::Move),
            "copy" => Some(Self::Copy),
            "trash" => Some(Self::Trash),
            _ => None,
        }
    }
}

/// 目标冲突的处理方式。当前执行器只开放 `Abort`，其余策略须单独验收后启用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    Abort,
    Skip,
    KeepBoth,
}

impl ConflictPolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Abort => "abort",
            Self::Skip => "skip",
            Self::KeepBoth => "keep_both",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "abort" => Some(Self::Abort),
            "skip" => Some(Self::Skip),
            "keep_both" => Some(Self::KeepBoth),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Draft,
    Ready,
    Expired,
    Executing,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub id: PlanId,
    pub library_id: LibraryId,
    pub operation_kind: FileOperationKind,
    pub conflict_policy: ConflictPolicy,
    pub status: PlanStatus,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Draft,
    Planned,
    PreflightPassed,
    IntentLogged,
    Executing,
    Published,
    Verified,
    Completed,
    Conflict,
    Failed,
    Cancelled,
    RecoveryNeeded,
    RollbackPending,
    RolledBack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    pub id: OperationId,
    pub plan_id: PlanId,
    pub status: OperationStatus,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    PauseRequested,
    Paused,
    CancelRequested,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub kind: String,
    pub status: JobStatus,
    pub progress_current: u64,
    pub progress_total: Option<u64>,
    pub created_at_ms: i64,
}
