use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use guixu_analysis::{
    DuplicateInput, DuplicateProgress, EXACT_HASH_ALGORITHM, FileCategory, RetentionInput,
    SimilarityHashInput, TEXT_MINHASH_COMPONENTS, classify_file,
    find_exact_duplicates_cached_controlled, find_similarity_candidates, image_hashes_from_path,
    minhash_similarity, rank_duplicate_retention, text_minhash, text_simhash, unique_filename,
};
use guixu_domain::{ConflictPolicy, FileOperationKind, FileSnapshot, JobStatus, RuntimeInfo};
use guixu_indexer::{PlatformWatcher, ReconcileRequest, ScanOptions, reconcile_snapshot};
use guixu_operations::{
    CopyRequest, RenameRequest, audit_incomplete_operations, execute_stored_plan, plan_copies,
    plan_renames, plan_trash, undo_stored_operation,
};
use guixu_platform::{FileLaunchAction, launch_file, observe_file};
use guixu_storage::{
    ClaimedJob, Database, FileFeature, FileHashCache, FilePageCursor, IndexedFile, NewJob, NewPlan,
    NewPlanItem, StoredOperation, parse_search_query,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

struct DesktopState {
    database: Mutex<Database>,
    database_path: PathBuf,
    watchers: Mutex<HashMap<String, PlatformWatcher>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryView {
    id: String,
    name: String,
    root_path: String,
    scan_job_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilesPageRequest {
    library_id: String,
    after_path: Option<String>,
    after_id: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileView {
    id: String,
    name: String,
    path: String,
    size: u64,
    modified_at_ns: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FilesPageView {
    items: Vec<FileView>,
    next_path: Option<String>,
    next_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryOverviewView {
    total_files: u64,
    total_bytes: u64,
    latest_modified_at_ns: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewView {
    id: String,
    name: String,
    path: String,
    size: u64,
    kind: String,
    text: Option<String>,
    truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct JobView {
    id: String,
    kind: String,
    status: String,
    progress_current: u64,
    progress_total: Option<u64>,
    updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SmartFolderView {
    id: String,
    name: String,
    query: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
struct AppSettingsView {
    restore_last_library: bool,
    default_view_mode: String,
    default_sort_key: String,
    default_sort_direction: String,
    page_size: usize,
    refresh_interval_ms: u64,
    show_full_paths: bool,
    density: String,
    show_overview: bool,
    show_task_center: bool,
    file_size_unit: String,
    date_format: String,
    reduce_motion: bool,
}

impl Default for AppSettingsView {
    fn default() -> Self {
        Self {
            restore_last_library: true,
            default_view_mode: "list".to_owned(),
            default_sort_key: "name".to_owned(),
            default_sort_direction: "asc".to_owned(),
            page_size: 100,
            refresh_interval_ms: 2_000,
            show_full_paths: true,
            density: "comfortable".to_owned(),
            show_overview: true,
            show_task_center: true,
            file_size_unit: "binary".to_owned(),
            date_format: "locale".to_owned(),
            reduce_motion: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScanPayload {
    library_id: String,
    root_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DuplicatePayload {
    library_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SimilarityPayload {
    library_id: String,
    content_kind: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatePlanRequest {
    library_id: String,
    file_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameItemRequest {
    file_id: String,
    new_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateRenamePlanRequest {
    library_id: String,
    items: Vec<RenameItemRequest>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanItemView {
    ordinal: i64,
    file_id: String,
    source_path: String,
    target_path: String,
    category: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanView {
    id: String,
    operation_kind: String,
    expires_at_ms: i64,
    items: Vec<PlanItemView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionView {
    operation_id: String,
    completed_items: usize,
    total_items: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationItemView {
    ordinal: i64,
    status: String,
    source_path: String,
    target_path: String,
    error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationView {
    id: String,
    operation_kind: String,
    status: String,
    created_at_ms: i64,
    completed_at_ms: Option<i64>,
    error_message: Option<String>,
    items: Vec<OperationItemView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateGroupView {
    id: String,
    size: u64,
    potential_savings: u64,
    paths: Vec<String>,
    suggested_keep: String,
    files: Vec<DuplicateFileView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateFileView {
    file_id: String,
    path: String,
    retention_score: i32,
    reasons: Vec<String>,
    suggested_keep: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateReportView {
    input_files: usize,
    quick_fingerprinted_files: usize,
    fully_hashed_files: usize,
    quick_cache_hits: usize,
    full_cache_hits: usize,
    skipped_files: usize,
    groups: Vec<DuplicateGroupView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SimilarContentPairView {
    left_file_id: String,
    left_path: String,
    right_file_id: String,
    right_path: String,
    hamming_distance: u32,
    secondary_distance: Option<u32>,
    secondary_similarity: f32,
    similarity: f32,
    verification: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SimilarContentReportView {
    content_kind: String,
    input_files: usize,
    computed_features: usize,
    cache_hits: usize,
    skipped_files: usize,
    candidate_pairs: usize,
    secondary_verified_pairs: usize,
    feature_ms: u64,
    candidate_ms: u64,
    algorithm_profile: String,
    pairs: Vec<SimilarContentPairView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalDataStatusView {
    indexed_files: u64,
    indexed_bytes: u64,
    cached_files: u64,
    fully_hashed_files: u64,
}

#[tauri::command]
fn runtime_info() -> RuntimeInfo {
    RuntimeInfo::current()
}

#[tauri::command]
fn app_settings(state: State<'_, DesktopState>) -> Result<AppSettingsView, String> {
    let database = lock_database(&state)?;
    let Some(json) = database.app_setting("desktop").map_err(display_error)? else {
        return Ok(AppSettingsView::default());
    };
    serde_json::from_str(&json).map_err(display_error)
}

#[tauri::command]
fn save_app_settings(
    state: State<'_, DesktopState>,
    settings: AppSettingsView,
) -> Result<AppSettingsView, String> {
    if !matches!(settings.default_view_mode.as_str(), "list" | "grid") {
        return Err("默认视图只能是列表或网格".to_owned());
    }
    if !matches!(
        settings.default_sort_key.as_str(),
        "name" | "size" | "modified"
    ) {
        return Err("默认排序字段无效".to_owned());
    }
    if !matches!(settings.default_sort_direction.as_str(), "asc" | "desc") {
        return Err("默认排序方向无效".to_owned());
    }
    if !matches!(settings.page_size, 50 | 100 | 200) {
        return Err("分页数量只能是 50、100 或 200".to_owned());
    }
    if !matches!(settings.refresh_interval_ms, 2_000 | 5_000 | 10_000) {
        return Err("刷新频率只能是 2、5 或 10 秒".to_owned());
    }
    if !matches!(settings.density.as_str(), "comfortable" | "compact") {
        return Err("界面密度无效".to_owned());
    }
    if !matches!(settings.file_size_unit.as_str(), "binary" | "decimal") {
        return Err("文件大小单位无效".to_owned());
    }
    if !matches!(settings.date_format.as_str(), "locale" | "iso") {
        return Err("日期格式无效".to_owned());
    }
    let json = serde_json::to_string(&settings).map_err(display_error)?;
    lock_database(&state)?
        .save_app_setting("desktop", &json, now_ms())
        .map_err(display_error)?;
    Ok(settings)
}

#[tauri::command]
fn local_data_status(
    state: State<'_, DesktopState>,
    library_id: String,
) -> Result<LocalDataStatusView, String> {
    let database = lock_database(&state)?;
    database
        .library_root(&library_id)
        .map_err(display_error)?
        .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
    let overview = database
        .library_overview(&library_id)
        .map_err(display_error)?;
    let hashes = database
        .hash_cache_overview(&library_id, EXACT_HASH_ALGORITHM)
        .map_err(display_error)?;
    Ok(LocalDataStatusView {
        indexed_files: overview.total_files,
        indexed_bytes: overview.total_bytes,
        cached_files: hashes.cached_files,
        fully_hashed_files: hashes.fully_hashed_files,
    })
}

#[tauri::command]
fn clear_hash_cache(state: State<'_, DesktopState>, library_id: String) -> Result<usize, String> {
    let database = lock_database(&state)?;
    database
        .library_root(&library_id)
        .map_err(display_error)?
        .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
    database
        .clear_hash_cache(&library_id)
        .map_err(display_error)
}

#[tauri::command]
fn active_library(state: State<'_, DesktopState>) -> Result<Option<LibraryView>, String> {
    let database = lock_database(&state)?;
    database
        .latest_library_root()
        .map(|record| {
            record.map(|record| LibraryView {
                id: record.library_id,
                name: record.name,
                root_path: record.root_path,
                scan_job_id: None,
            })
        })
        .map_err(display_error)
}

#[tauri::command]
async fn select_library_folder(app: AppHandle) -> Result<Option<LibraryView>, String> {
    let Some(selected) = app.dialog().file().blocking_pick_folder() else {
        return Ok(None);
    };
    let selected = selected.into_path().map_err(|error| error.to_string())?;
    let canonical = selected.canonicalize().map_err(display_error)?;
    if !canonical.is_dir() {
        return Err("所选路径不是文件夹".to_owned());
    }
    let root_path = canonical
        .to_str()
        .ok_or_else(|| "所选文件夹路径不是有效 UTF-8，当前版本无法安全记录".to_owned())?
        .to_owned();
    let timestamp = now_ms();
    let library_id = unique_id("library", timestamp);
    let root_id = unique_id("root", timestamp);
    let scan_job_id = unique_id("scan", timestamp);
    let name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("文件资料库")
        .to_owned();
    // 先确认平台监听器可建立，避免注册资料库后留下永远 queued 的扫描任务。
    let watcher = PlatformWatcher::start(&canonical).map_err(display_error)?;
    let (record, scan_job_id) = {
        let state = app.state::<DesktopState>();
        let mut database = lock_database(&state)?;
        let record = database
            .register_library_root(&library_id, &root_id, &name, &root_path, timestamp)
            .map_err(display_error)?;
        let payload = serde_json::to_string(&ScanPayload {
            library_id: record.library_id.clone(),
            root_path: record.root_path.clone(),
        })
        .map_err(display_error)?;
        let scan_job_id = database
            .enqueue_or_reuse_job(&NewJob {
                id: scan_job_id.clone(),
                kind: "library_scan".to_owned(),
                payload_json: payload,
                priority: 100,
                progress_total: None,
                created_at_ms: timestamp,
            })
            .map_err(display_error)?;
        (record, scan_job_id)
    };
    {
        let state = app.state::<DesktopState>();
        state
            .watchers
            .lock()
            .map_err(|_| "文件事件监听器锁已损坏".to_owned())?
            .insert(record.library_id.clone(), watcher);
    }
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || run_one_job(worker_app));
    Ok(Some(LibraryView {
        id: record.library_id,
        name: record.name,
        root_path: record.root_path,
        scan_job_id: Some(scan_job_id),
    }))
}

#[tauri::command]
fn rescan_library(app: AppHandle, library_id: String) -> Result<String, String> {
    let timestamp = now_ms();
    let mut job_id = unique_id("scan", timestamp);
    {
        let state = app.state::<DesktopState>();
        let mut database = lock_database(&state)?;
        let root = database
            .library_root(&library_id)
            .map_err(display_error)?
            .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
        let payload = serde_json::to_string(&ScanPayload {
            library_id,
            root_path: root.root_path,
        })
        .map_err(display_error)?;
        job_id = database
            .enqueue_or_reuse_job(&NewJob {
                id: job_id.clone(),
                kind: "library_scan".to_owned(),
                payload_json: payload,
                priority: 100,
                progress_total: None,
                created_at_ms: timestamp,
            })
            .map_err(display_error)?;
    }
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || run_one_job(worker_app));
    Ok(job_id)
}

#[tauri::command]
fn files_page(
    state: State<'_, DesktopState>,
    request: FilesPageRequest,
) -> Result<FilesPageView, String> {
    let cursor = match (request.after_path, request.after_id) {
        (Some(path), Some(id)) => Some(FilePageCursor { path, id }),
        _ => None,
    };
    let database = lock_database(&state)?;
    let page = database
        .list_files_page(
            &request.library_id,
            cursor.as_ref(),
            request.limit.unwrap_or(100).clamp(1, 500),
        )
        .map_err(display_error)?;
    let (next_path, next_id) = page
        .next_cursor
        .map(|cursor| (Some(cursor.path), Some(cursor.id)))
        .unwrap_or((None, None));
    Ok(FilesPageView {
        items: page
            .items
            .into_iter()
            .map(|file| FileView {
                name: Path::new(&file.current_path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(&file.current_path)
                    .to_owned(),
                id: file.id,
                path: file.current_path,
                size: file.size,
                modified_at_ns: file.modified_at_ns,
            })
            .collect(),
        next_path,
        next_id,
    })
}

#[tauri::command]
fn library_overview(
    state: State<'_, DesktopState>,
    library_id: String,
) -> Result<LibraryOverviewView, String> {
    let database = lock_database(&state)?;
    let overview = database
        .library_overview(&library_id)
        .map_err(display_error)?;
    Ok(LibraryOverviewView {
        total_files: overview.total_files,
        total_bytes: overview.total_bytes,
        latest_modified_at_ns: overview.latest_modified_at_ns,
    })
}

#[tauri::command]
fn preview_file(
    state: State<'_, DesktopState>,
    library_id: String,
    file_id: String,
) -> Result<PreviewView, String> {
    let database = lock_database(&state)?;
    let (indexed, canonical) = authorized_indexed_file(&database, &library_id, &file_id)?;
    drop(database);
    let name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("未命名文件")
        .to_owned();
    let extension = canonical
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let text_kind = matches!(
        extension.as_str(),
        "txt"
            | "md"
            | "json"
            | "csv"
            | "tsv"
            | "log"
            | "rs"
            | "py"
            | "js"
            | "ts"
            | "css"
            | "html"
            | "xml"
            | "yaml"
            | "yml"
            | "toml"
    );
    let (text, truncated) = if text_kind {
        const MAX_PREVIEW_BYTES: usize = 256 * 1024;
        let mut bytes = Vec::with_capacity(MAX_PREVIEW_BYTES + 1);
        File::open(&canonical)
            .map_err(display_error)?
            .take((MAX_PREVIEW_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(display_error)?;
        let truncated = bytes.len() > MAX_PREVIEW_BYTES;
        bytes.truncate(MAX_PREVIEW_BYTES);
        (
            Some(String::from_utf8_lossy(&bytes).into_owned()),
            truncated,
        )
    } else {
        (None, false)
    };
    Ok(PreviewView {
        id: indexed.id,
        name,
        path: indexed.current_path,
        size: indexed.size,
        kind: if text_kind { "text" } else { "metadata" }.to_owned(),
        text,
        truncated,
    })
}

#[tauri::command]
fn open_file(
    state: State<'_, DesktopState>,
    library_id: String,
    file_id: String,
) -> Result<(), String> {
    launch_indexed_file(&state, &library_id, &file_id, FileLaunchAction::Open)
}

#[tauri::command]
fn reveal_file(
    state: State<'_, DesktopState>,
    library_id: String,
    file_id: String,
) -> Result<(), String> {
    launch_indexed_file(&state, &library_id, &file_id, FileLaunchAction::Reveal)
}

fn launch_indexed_file(
    state: &State<'_, DesktopState>,
    library_id: &str,
    file_id: &str,
    action: FileLaunchAction,
) -> Result<(), String> {
    let database = lock_database(state)?;
    let (_, canonical) = authorized_indexed_file(&database, library_id, file_id)?;
    drop(database);
    launch_file(&canonical, action).map_err(display_error)
}

fn authorized_indexed_file(
    database: &Database,
    library_id: &str,
    file_id: &str,
) -> Result<(IndexedFile, PathBuf), String> {
    let library = database
        .library_root(library_id)
        .map_err(display_error)?
        .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
    let indexed = database
        .indexed_file(library_id, file_id)
        .map_err(display_error)?
        .ok_or_else(|| "文件不在当前索引中".to_owned())?;
    let canonical = validate_authorized_path(
        Path::new(&library.root_path),
        Path::new(&indexed.current_path),
    )?;
    Ok((indexed, canonical))
}

fn validate_authorized_path(root_path: &Path, current_path: &Path) -> Result<PathBuf, String> {
    let root = root_path.canonicalize().map_err(display_error)?;
    let path = current_path;
    let metadata = fs::symlink_metadata(path).map_err(display_error)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("文件已经离开授权根目录或类型发生变化".to_owned());
    }
    let canonical = path.canonicalize().map_err(display_error)?;
    if !canonical.starts_with(&root) {
        return Err("文件已经离开授权根目录".to_owned());
    }
    Ok(canonical)
}

#[tauri::command]
fn jobs(state: State<'_, DesktopState>) -> Result<Vec<JobView>, String> {
    let database = lock_database(&state)?;
    database
        .list_jobs(50)
        .map(|jobs| {
            jobs.into_iter()
                .map(|job| JobView {
                    id: job.id,
                    kind: job.kind,
                    status: job.status,
                    progress_current: job.progress_current,
                    progress_total: job.progress_total,
                    updated_at_ms: job.updated_at_ms,
                })
                .collect()
        })
        .map_err(display_error)
}

#[tauri::command]
fn poll_file_events(app: AppHandle) -> Result<(), String> {
    schedule_watch_reconciliation(&app)
}

#[tauri::command]
fn pause_job(state: State<'_, DesktopState>, job_id: String) -> Result<(), String> {
    lock_database(&state)?
        .request_job_pause(&job_id, now_ms())
        .map_err(display_error)
}

#[tauri::command]
fn cancel_job(state: State<'_, DesktopState>, job_id: String) -> Result<(), String> {
    lock_database(&state)?
        .request_job_cancel(&job_id, now_ms())
        .map_err(display_error)
}

#[tauri::command]
fn resume_job(app: AppHandle, job_id: String) -> Result<(), String> {
    {
        let state = app.state::<DesktopState>();
        lock_database(&state)?
            .resume_job(&job_id, now_ms())
            .map_err(display_error)?;
    }
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || run_one_job(worker_app));
    Ok(())
}

#[tauri::command]
fn search_files(
    state: State<'_, DesktopState>,
    library_id: String,
    query: String,
) -> Result<Vec<FileView>, String> {
    let database = lock_database(&state)?;
    database
        .search_files(&library_id, &query, 200)
        .map(|hits| {
            hits.into_iter()
                .map(|hit| FileView {
                    name: Path::new(&hit.file.current_path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or(&hit.file.current_path)
                        .to_owned(),
                    id: hit.file.id,
                    path: hit.file.current_path,
                    size: hit.file.size,
                    modified_at_ns: hit.file.modified_at_ns,
                })
                .collect()
        })
        .map_err(display_error)
}

#[tauri::command]
fn smart_folders(
    state: State<'_, DesktopState>,
    library_id: String,
) -> Result<Vec<SmartFolderView>, String> {
    let database = lock_database(&state)?;
    database
        .list_smart_folders(&library_id)
        .map(|folders| {
            folders
                .into_iter()
                .map(|folder| SmartFolderView {
                    id: folder.id,
                    name: folder.name,
                    query: folder.query,
                })
                .collect()
        })
        .map_err(display_error)
}

#[tauri::command]
fn save_smart_folder(
    state: State<'_, DesktopState>,
    library_id: String,
    name: String,
    query: String,
) -> Result<SmartFolderView, String> {
    if name.trim().is_empty() || query.trim().is_empty() {
        return Err("名称和搜索条件不能为空".to_owned());
    }
    parse_search_query(query.trim()).map_err(display_error)?;
    let timestamp = now_ms();
    let folder = guixu_storage::SmartFolder {
        id: unique_id("smart", timestamp),
        library_id,
        name: name.trim().chars().take(80).collect(),
        query: query.trim().chars().take(300).collect(),
        created_at_ms: timestamp,
    };
    let database = lock_database(&state)?;
    database.save_smart_folder(&folder).map_err(display_error)?;
    Ok(SmartFolderView {
        id: folder.id,
        name: folder.name,
        query: folder.query,
    })
}

#[tauri::command]
fn delete_smart_folder(
    state: State<'_, DesktopState>,
    library_id: String,
    id: String,
) -> Result<bool, String> {
    let database = lock_database(&state)?;
    database
        .delete_smart_folder(&library_id, &id)
        .map_err(display_error)
}

#[tauri::command]
fn create_organize_plan(
    state: State<'_, DesktopState>,
    request: CreatePlanRequest,
) -> Result<PlanView, String> {
    if request.file_ids.is_empty() {
        return Err("请先选择至少一个文件".to_owned());
    }
    if request.file_ids.len() > 500 {
        return Err("单次最多整理 500 个文件".to_owned());
    }
    let mut requested = request
        .file_ids
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    requested.sort();
    let database = lock_database(&state)?;
    let library = database
        .library_root(&request.library_id)
        .map_err(display_error)?
        .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
    let root = PathBuf::from(&library.root_path)
        .canonicalize()
        .map_err(display_error)?;
    let output_root = root.join("归序整理");
    let mut reserved_by_directory: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    let mut stored_items = Vec::new();
    let mut view_items = Vec::new();

    for file_id in requested {
        let indexed = database
            .indexed_file(&request.library_id, &file_id)
            .map_err(display_error)?
            .ok_or_else(|| format!("文件已不在当前索引中：{file_id}"))?;
        let source = PathBuf::from(&indexed.current_path)
            .canonicalize()
            .map_err(display_error)?;
        if !source.starts_with(&root) || source.starts_with(&output_root) {
            return Err(format!("文件不在可整理范围内：{}", source.display()));
        }
        let observed = observe_file(&source).map_err(display_error)?;
        let mut header = [0_u8; 16];
        let header_length = File::open(&source)
            .and_then(|mut file| file.read(&mut header))
            .map_err(display_error)?;
        let category = category_label(classify_file(&source, &header[..header_length]).category);
        let target_directory = output_root.join(category);
        if !reserved_by_directory.contains_key(&target_directory) {
            let existing = if target_directory.is_dir() {
                fs::read_dir(&target_directory)
                    .map_err(display_error)?
                    .map(|entry| entry.map_err(display_error))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .filter_map(|entry| entry.file_name().into_string().ok())
                    .collect()
            } else {
                HashSet::new()
            };
            reserved_by_directory.insert(target_directory.clone(), existing);
        }
        let reserved = reserved_by_directory
            .get_mut(&target_directory)
            .expect("target directory inventory exists");
        let desired = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "当前版本不能安全整理非 UTF-8 文件名".to_owned())?;
        let target_name = unique_filename(desired, reserved);
        reserved.insert(target_name.clone());
        let target = target_directory.join(target_name);
        let ordinal = i64::try_from(stored_items.len()).map_err(display_error)?;
        stored_items.push(NewPlanItem {
            ordinal,
            file_id: indexed.id.clone(),
            source_path: source.to_string_lossy().into_owned(),
            target_path: target.to_string_lossy().into_owned(),
            expected_identity: observed.identity,
            expected_snapshot: observed.snapshot,
        });
        view_items.push(PlanItemView {
            ordinal,
            file_id: indexed.id,
            source_path: source.to_string_lossy().into_owned(),
            target_path: target.to_string_lossy().into_owned(),
            category: category.to_owned(),
        });
    }
    view_items.sort_by(|left, right| left.source_path.cmp(&right.source_path));
    stored_items.sort_by(|left, right| left.source_path.cmp(&right.source_path));
    for (ordinal, item) in stored_items.iter_mut().enumerate() {
        item.ordinal = i64::try_from(ordinal).map_err(display_error)?;
    }
    for (ordinal, item) in view_items.iter_mut().enumerate() {
        item.ordinal = i64::try_from(ordinal).map_err(display_error)?;
    }
    let timestamp = now_ms();
    let expires_at_ms = timestamp.saturating_add(15 * 60 * 1_000);
    let plan_id = unique_id("plan", timestamp);
    database
        .create_plan(NewPlan {
            id: &plan_id,
            library_id: &request.library_id,
            operation_kind: FileOperationKind::Organize,
            conflict_policy: ConflictPolicy::Abort,
            created_at_ms: timestamp,
            expires_at_ms,
            items: &stored_items,
        })
        .map_err(display_error)?;
    Ok(PlanView {
        id: plan_id,
        operation_kind: FileOperationKind::Organize.as_str().to_owned(),
        expires_at_ms,
        items: view_items,
    })
}

#[tauri::command]
fn create_rename_plan(
    state: State<'_, DesktopState>,
    request: CreateRenamePlanRequest,
) -> Result<PlanView, String> {
    if request.items.is_empty() {
        return Err("请先选择至少一个文件".to_owned());
    }
    if request.items.len() > 500 {
        return Err("单次最多重命名 500 个文件".to_owned());
    }
    let database = lock_database(&state)?;
    let library = database
        .library_root(&request.library_id)
        .map_err(display_error)?
        .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
    let root = PathBuf::from(&library.root_path)
        .canonicalize()
        .map_err(display_error)?;
    let mut seen_ids = HashSet::with_capacity(request.items.len());
    let mut rename_requests = Vec::with_capacity(request.items.len());
    for item in request.items {
        if !seen_ids.insert(item.file_id.clone()) {
            return Err(format!("重命名请求包含重复文件：{}", item.file_id));
        }
        let indexed = database
            .indexed_file(&request.library_id, &item.file_id)
            .map_err(display_error)?
            .ok_or_else(|| format!("文件已不在当前索引中：{}", item.file_id))?;
        let source = validate_authorized_path(&root, Path::new(&indexed.current_path))?;
        let observed = observe_file(&source).map_err(display_error)?;
        rename_requests.push(RenameRequest {
            file_id: indexed.id,
            source,
            new_name: item.new_name,
            expected_identity: observed.identity,
            expected_snapshot: observed.snapshot,
        });
    }
    let planned = plan_renames(&root, &rename_requests).map_err(display_error)?;
    let stored_items = planned
        .iter()
        .enumerate()
        .map(|(ordinal, item)| {
            Ok(NewPlanItem {
                ordinal: i64::try_from(ordinal).map_err(display_error)?,
                file_id: item.file_id.clone(),
                source_path: item.source.to_string_lossy().into_owned(),
                target_path: item.target.to_string_lossy().into_owned(),
                expected_identity: item.expected_identity.clone(),
                expected_snapshot: item.expected_snapshot.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let view_items = stored_items
        .iter()
        .map(|item| PlanItemView {
            ordinal: item.ordinal,
            file_id: item.file_id.clone(),
            source_path: item.source_path.clone(),
            target_path: item.target_path.clone(),
            category: "重命名".to_owned(),
        })
        .collect();
    let timestamp = now_ms();
    let expires_at_ms = timestamp.saturating_add(15 * 60 * 1_000);
    let plan_id = unique_id("rename-plan", timestamp);
    database
        .create_plan(NewPlan {
            id: &plan_id,
            library_id: &request.library_id,
            operation_kind: FileOperationKind::Rename,
            conflict_policy: ConflictPolicy::Abort,
            created_at_ms: timestamp,
            expires_at_ms,
            items: &stored_items,
        })
        .map_err(display_error)?;
    Ok(PlanView {
        id: plan_id,
        operation_kind: FileOperationKind::Rename.as_str().to_owned(),
        expires_at_ms,
        items: view_items,
    })
}

#[tauri::command]
fn create_copy_plan(
    app: AppHandle,
    request: CreatePlanRequest,
) -> Result<Option<PlanView>, String> {
    create_destination_plan(app, request, FileOperationKind::Copy)
}

#[tauri::command]
fn create_move_plan(
    app: AppHandle,
    request: CreatePlanRequest,
) -> Result<Option<PlanView>, String> {
    create_destination_plan(app, request, FileOperationKind::Move)
}

fn create_destination_plan(
    app: AppHandle,
    request: CreatePlanRequest,
    operation_kind: FileOperationKind,
) -> Result<Option<PlanView>, String> {
    let action = if operation_kind == FileOperationKind::Copy {
        "复制"
    } else {
        "移动"
    };
    if request.file_ids.is_empty() {
        return Err("请先选择至少一个文件".to_owned());
    }
    if request.file_ids.len() > 500 {
        return Err(format!("单次最多{action} 500 个文件"));
    }
    // 目标只来自系统文件夹选择器，页面不接收也不拼接任意路径。
    let Some(selected) = app.dialog().file().blocking_pick_folder() else {
        return Ok(None);
    };
    let destination = selected.into_path().map_err(|error| error.to_string())?;
    let state = app.state::<DesktopState>();
    let database = lock_database(&state)?;
    let library = database
        .library_root(&request.library_id)
        .map_err(display_error)?
        .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
    let root = PathBuf::from(&library.root_path)
        .canonicalize()
        .map_err(display_error)?;
    let mut seen_ids = HashSet::with_capacity(request.file_ids.len());
    let mut copy_requests = Vec::with_capacity(request.file_ids.len());
    for file_id in request.file_ids {
        if !seen_ids.insert(file_id.clone()) {
            return Err(format!("{action}请求包含重复文件：{file_id}"));
        }
        let indexed = database
            .indexed_file(&request.library_id, &file_id)
            .map_err(display_error)?
            .ok_or_else(|| format!("文件已不在当前索引中：{file_id}"))?;
        let source = validate_authorized_path(&root, Path::new(&indexed.current_path))?;
        let observed = observe_file(&source).map_err(display_error)?;
        copy_requests.push(CopyRequest {
            file_id: indexed.id,
            source,
            expected_identity: observed.identity,
            expected_snapshot: observed.snapshot,
        });
    }
    let planned = plan_copies(&root, &destination, &copy_requests).map_err(display_error)?;
    let stored_items = planned
        .iter()
        .enumerate()
        .map(|(ordinal, item)| {
            Ok(NewPlanItem {
                ordinal: i64::try_from(ordinal).map_err(display_error)?,
                file_id: item.file_id.clone(),
                source_path: item.source.to_string_lossy().into_owned(),
                target_path: item.target.to_string_lossy().into_owned(),
                expected_identity: item.expected_identity.clone(),
                expected_snapshot: item.expected_snapshot.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let view_items = stored_items
        .iter()
        .map(|item| PlanItemView {
            ordinal: item.ordinal,
            file_id: item.file_id.clone(),
            source_path: item.source_path.clone(),
            target_path: item.target_path.clone(),
            category: if operation_kind == FileOperationKind::Copy {
                "安全复制".to_owned()
            } else {
                "安全移动".to_owned()
            },
        })
        .collect();
    let timestamp = now_ms();
    let expires_at_ms = timestamp.saturating_add(15 * 60 * 1_000);
    let plan_id = unique_id(&format!("{}-plan", operation_kind.as_str()), timestamp);
    database
        .create_plan(NewPlan {
            id: &plan_id,
            library_id: &request.library_id,
            operation_kind,
            conflict_policy: ConflictPolicy::Abort,
            created_at_ms: timestamp,
            expires_at_ms,
            items: &stored_items,
        })
        .map_err(display_error)?;
    Ok(Some(PlanView {
        id: plan_id,
        operation_kind: operation_kind.as_str().to_owned(),
        expires_at_ms,
        items: view_items,
    }))
}

#[tauri::command]
fn create_trash_plan(
    state: State<'_, DesktopState>,
    request: CreatePlanRequest,
) -> Result<PlanView, String> {
    if request.file_ids.is_empty() {
        return Err("请先选择至少一个文件".to_owned());
    }
    if request.file_ids.len() > 500 {
        return Err("单次最多移入废纸篓 500 个文件".to_owned());
    }
    let database = lock_database(&state)?;
    let library = database
        .library_root(&request.library_id)
        .map_err(display_error)?
        .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
    let root = PathBuf::from(&library.root_path)
        .canonicalize()
        .map_err(display_error)?;
    let mut seen_ids = HashSet::with_capacity(request.file_ids.len());
    let mut trash_requests = Vec::with_capacity(request.file_ids.len());
    for file_id in request.file_ids {
        if !seen_ids.insert(file_id.clone()) {
            return Err(format!("废纸篓请求包含重复文件：{file_id}"));
        }
        let indexed = database
            .indexed_file(&request.library_id, &file_id)
            .map_err(display_error)?
            .ok_or_else(|| format!("文件已不在当前索引中：{file_id}"))?;
        let source = validate_authorized_path(&root, Path::new(&indexed.current_path))?;
        let observed = observe_file(&source).map_err(display_error)?;
        trash_requests.push(CopyRequest {
            file_id: indexed.id,
            source,
            expected_identity: observed.identity,
            expected_snapshot: observed.snapshot,
        });
    }
    let timestamp = now_ms();
    let plan_id = unique_id("trash-plan", timestamp);
    let planned = plan_trash(&root, &plan_id, &trash_requests).map_err(display_error)?;
    let stored_items = planned
        .iter()
        .enumerate()
        .map(|(ordinal, item)| {
            Ok(NewPlanItem {
                ordinal: i64::try_from(ordinal).map_err(display_error)?,
                file_id: item.file_id.clone(),
                source_path: item.source.to_string_lossy().into_owned(),
                target_path: item.target.to_string_lossy().into_owned(),
                expected_identity: item.expected_identity.clone(),
                expected_snapshot: item.expected_snapshot.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let view_items = stored_items
        .iter()
        .map(|item| PlanItemView {
            ordinal: item.ordinal,
            file_id: item.file_id.clone(),
            source_path: item.source_path.clone(),
            target_path: item.target_path.clone(),
            category: "可恢复删除".to_owned(),
        })
        .collect();
    let expires_at_ms = timestamp.saturating_add(15 * 60 * 1_000);
    database
        .create_plan(NewPlan {
            id: &plan_id,
            library_id: &request.library_id,
            operation_kind: FileOperationKind::Trash,
            conflict_policy: ConflictPolicy::Abort,
            created_at_ms: timestamp,
            expires_at_ms,
            items: &stored_items,
        })
        .map_err(display_error)?;
    Ok(PlanView {
        id: plan_id,
        operation_kind: FileOperationKind::Trash.as_str().to_owned(),
        expires_at_ms,
        items: view_items,
    })
}

#[tauri::command]
async fn execute_organize_plan(app: AppHandle, plan_id: String) -> Result<ExecutionView, String> {
    execute_file_plan(app, plan_id, FileOperationKind::Organize).await
}

#[tauri::command]
async fn execute_rename_plan(app: AppHandle, plan_id: String) -> Result<ExecutionView, String> {
    execute_file_plan(app, plan_id, FileOperationKind::Rename).await
}

#[tauri::command]
async fn execute_copy_plan(app: AppHandle, plan_id: String) -> Result<ExecutionView, String> {
    execute_file_plan(app, plan_id, FileOperationKind::Copy).await
}

#[tauri::command]
async fn execute_move_plan(app: AppHandle, plan_id: String) -> Result<ExecutionView, String> {
    execute_file_plan(app, plan_id, FileOperationKind::Move).await
}

#[tauri::command]
async fn execute_trash_plan(app: AppHandle, plan_id: String) -> Result<ExecutionView, String> {
    execute_file_plan(app, plan_id, FileOperationKind::Trash).await
}

async fn execute_file_plan(
    app: AppHandle,
    plan_id: String,
    expected_kind: FileOperationKind,
) -> Result<ExecutionView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let timestamp = now_ms();
        let operation_id = unique_id("operation", timestamp);
        let mut database = open_worker_database(&app)?;
        let plan = database
            .plan(&plan_id)
            .map_err(display_error)?
            .ok_or_else(|| "文件操作计划不存在".to_owned())?;
        ensure_plan_kind(plan.operation_kind, expected_kind)?;
        let root = database
            .library_root(&plan.library_id)
            .map_err(display_error)?
            .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
        let report = execute_stored_plan(
            &mut database,
            Path::new(&root.root_path),
            &plan_id,
            &operation_id,
            timestamp,
        )
        .map_err(display_error)?;
        Ok(ExecutionView {
            operation_id: report.operation_id,
            completed_items: report.completed_items,
            total_items: report.total_items,
        })
    })
    .await
    .map_err(display_error)?
}

fn ensure_plan_kind(actual: FileOperationKind, expected: FileOperationKind) -> Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "计划类型不匹配：期望 {}，实际 {}",
            expected.as_str(),
            actual.as_str()
        ))
    }
}

#[tauri::command]
fn operation_history(
    state: State<'_, DesktopState>,
    library_id: String,
) -> Result<Vec<OperationView>, String> {
    let database = lock_database(&state)?;
    database
        .operation_history(&library_id, 50)
        .map(|operations| operations.into_iter().map(operation_view).collect())
        .map_err(display_error)
}

const TEXT_SIMHASH_VERSION: &str = "text-simhash-char3-v1";
const TEXT_MINHASH_VERSION: &str = "text-minhash-char5x32-v1";
const IMAGE_DHASH_VERSION: &str = "image-dhash-gray9x8-v1";
const IMAGE_PHASH_VERSION: &str = "image-phash-dct32x32-low8-v1";

fn u64_feature(feature: Option<FileFeature>) -> Option<u64> {
    let bytes = feature?.feature_blob;
    (bytes.len() == 8)
        .then(|| u64::from_le_bytes(bytes.as_slice().try_into().expect("validated eight bytes")))
}

fn minhash_feature(feature: Option<FileFeature>) -> Option<[u64; TEXT_MINHASH_COMPONENTS]> {
    let bytes = feature?.feature_blob;
    if bytes.len() != TEXT_MINHASH_COMPONENTS * 8 {
        return None;
    }
    Some(std::array::from_fn(|index| {
        let offset = index * 8;
        u64::from_le_bytes(
            bytes[offset..offset + 8]
                .try_into()
                .expect("eight-byte component"),
        )
    }))
}

fn minhash_blob(signature: &[u64; TEXT_MINHASH_COMPONENTS]) -> Vec<u8> {
    signature
        .iter()
        .flat_map(|component| component.to_le_bytes())
        .collect()
}

fn compute_similar_texts(
    app: &AppHandle,
    library_id: &str,
    mut should_continue: impl FnMut(u64, u64) -> bool,
) -> Result<(SimilarContentReportView, bool), String> {
    let feature_started = Instant::now();
    let database = open_worker_database(app)?;
    let library = database
        .library_root(library_id)
        .map_err(display_error)?
        .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
    let root = PathBuf::from(library.root_path)
        .canonicalize()
        .map_err(display_error)?;
    let mut cursor = None;
    let mut files = HashMap::<String, IndexedFile>::new();
    let mut hashes = Vec::new();
    let mut minhashes = HashMap::<String, [u64; TEXT_MINHASH_COMPONENTS]>::new();
    let mut computed_features = 0_usize;
    let mut cache_hits = 0_usize;
    let mut skipped_files = 0_usize;
    let total_files = database
        .library_overview(library_id)
        .map_err(display_error)?
        .total_files;
    let mut processed_files = 0_u64;
    let mut interrupted = false;
    'pages: loop {
        let page = database
            .list_files_page(library_id, cursor.as_ref(), 500)
            .map_err(display_error)?;
        for file in page.items {
            processed_files = processed_files.saturating_add(1);
            if !should_continue(processed_files, total_files) {
                interrupted = true;
                break 'pages;
            }
            let path = PathBuf::from(&file.current_path);
            let extension = path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !matches!(
                extension.as_str(),
                "txt"
                    | "md"
                    | "log"
                    | "csv"
                    | "tsv"
                    | "json"
                    | "xml"
                    | "yaml"
                    | "yml"
                    | "toml"
                    | "rs"
                    | "py"
                    | "js"
                    | "ts"
                    | "css"
                    | "html"
            ) {
                continue;
            }
            let canonical = match path.canonicalize() {
                Ok(path) if path.starts_with(&root) => path,
                _ => {
                    skipped_files += 1;
                    continue;
                }
            };
            let cached_hash = u64_feature(
                database
                    .file_feature(&file, "text_simhash", TEXT_SIMHASH_VERSION)
                    .map_err(display_error)?,
            );
            let cached_minhash = minhash_feature(
                database
                    .file_feature(&file, "text_minhash", TEXT_MINHASH_VERSION)
                    .map_err(display_error)?,
            );
            let (hash, minhash) = if let (Some(hash), Some(minhash)) = (cached_hash, cached_minhash)
            {
                cache_hits += 1;
                (hash, minhash)
            } else {
                const MAX_TEXT_BYTES: u64 = 512 * 1024;
                let mut bytes = Vec::new();
                if File::open(&canonical)
                    .and_then(|file| file.take(MAX_TEXT_BYTES).read_to_end(&mut bytes))
                    .is_err()
                {
                    skipped_files += 1;
                    continue;
                }
                let text = String::from_utf8_lossy(&bytes);
                if text
                    .chars()
                    .filter(|character| character.is_alphanumeric())
                    .take(24)
                    .count()
                    < 24
                {
                    skipped_files += 1;
                    continue;
                }
                let Some(hash) = text_simhash(&text) else {
                    skipped_files += 1;
                    continue;
                };
                let Some(minhash) = text_minhash(&text) else {
                    skipped_files += 1;
                    continue;
                };
                database
                    .save_file_feature(&FileFeature {
                        file_id: file.id.clone(),
                        feature_kind: "text_simhash".to_owned(),
                        model_version: TEXT_SIMHASH_VERSION.to_owned(),
                        size: file.size,
                        modified_at_ns: file.modified_at_ns,
                        changed_at_ns: file.changed_at_ns,
                        dimensions: 64,
                        quantization: "binary64".to_owned(),
                        feature_blob: hash.to_le_bytes().to_vec(),
                        created_at_ms: now_ms(),
                    })
                    .map_err(display_error)?;
                database
                    .save_file_feature(&FileFeature {
                        file_id: file.id.clone(),
                        feature_kind: "text_minhash".to_owned(),
                        model_version: TEXT_MINHASH_VERSION.to_owned(),
                        size: file.size,
                        modified_at_ns: file.modified_at_ns,
                        changed_at_ns: file.changed_at_ns,
                        dimensions: TEXT_MINHASH_COMPONENTS as u32,
                        quantization: "u64x32".to_owned(),
                        feature_blob: minhash_blob(&minhash),
                        created_at_ms: now_ms(),
                    })
                    .map_err(display_error)?;
                computed_features += 1;
                (hash, minhash)
            };
            hashes.push(SimilarityHashInput {
                id: file.id.clone(),
                hash,
            });
            minhashes.insert(file.id.clone(), minhash);
            files.insert(file.id.clone(), file);
        }
        let Some(next) = page.next_cursor else { break };
        cursor = Some(next);
    }
    let feature_ms = feature_started.elapsed().as_millis() as u64;
    let candidate_started = Instant::now();
    let candidates = find_similarity_candidates(&hashes, 8).map_err(display_error)?;
    let candidate_pairs = candidates.len();
    let pairs = candidates
        .into_iter()
        .filter_map(|candidate| {
            let left = files.get(&candidate.left_id)?;
            let right = files.get(&candidate.right_id)?;
            let minhash_similarity = minhash_similarity(
                minhashes.get(&candidate.left_id)?,
                minhashes.get(&candidate.right_id)?,
            );
            if minhash_similarity < 0.25 {
                return None;
            }
            Some(SimilarContentPairView {
                left_file_id: left.id.clone(),
                left_path: left.current_path.clone(),
                right_file_id: right.id.clone(),
                right_path: right.current_path.clone(),
                hamming_distance: candidate.hamming_distance,
                secondary_distance: None,
                secondary_similarity: minhash_similarity,
                similarity: candidate.similarity * 0.45 + minhash_similarity * 0.55,
                verification: "SimHash 候选 + MinHash 字符五元组复核".to_owned(),
            })
        })
        .collect::<Vec<_>>();
    let candidate_ms = candidate_started.elapsed().as_millis() as u64;
    let secondary_verified_pairs = pairs.len();
    Ok((
        SimilarContentReportView {
            content_kind: "text".to_owned(),
            input_files: hashes.len(),
            computed_features,
            cache_hits,
            skipped_files,
            candidate_pairs,
            secondary_verified_pairs,
            feature_ms,
            candidate_ms,
            algorithm_profile: "simhash64-mih3-r8+minhash32-v1".to_owned(),
            pairs,
        },
        interrupted,
    ))
}

fn compute_similar_images(
    app: &AppHandle,
    library_id: &str,
    mut should_continue: impl FnMut(u64, u64) -> bool,
) -> Result<(SimilarContentReportView, bool), String> {
    let feature_started = Instant::now();
    let database = open_worker_database(app)?;
    let library = database
        .library_root(library_id)
        .map_err(display_error)?
        .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
    let root = PathBuf::from(library.root_path)
        .canonicalize()
        .map_err(display_error)?;
    let mut cursor = None;
    let mut files = HashMap::<String, IndexedFile>::new();
    let mut hashes = Vec::new();
    let mut phashes = HashMap::<String, u64>::new();
    let mut computed_features = 0_usize;
    let mut cache_hits = 0_usize;
    let mut skipped_files = 0_usize;
    let total_files = database
        .library_overview(library_id)
        .map_err(display_error)?
        .total_files;
    let mut processed_files = 0_u64;
    let mut interrupted = false;
    'pages: loop {
        let page = database
            .list_files_page(library_id, cursor.as_ref(), 500)
            .map_err(display_error)?;
        for file in page.items {
            processed_files = processed_files.saturating_add(1);
            if !should_continue(processed_files, total_files) {
                interrupted = true;
                break 'pages;
            }
            let path = PathBuf::from(&file.current_path);
            let extension = path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "webp") {
                continue;
            }
            let canonical = match path.canonicalize() {
                Ok(path) if path.starts_with(&root) => path,
                _ => {
                    skipped_files += 1;
                    continue;
                }
            };
            let cached_dhash = u64_feature(
                database
                    .file_feature(&file, "image_dhash", IMAGE_DHASH_VERSION)
                    .map_err(display_error)?,
            );
            let cached_phash = u64_feature(
                database
                    .file_feature(&file, "image_phash", IMAGE_PHASH_VERSION)
                    .map_err(display_error)?,
            );
            let (hash, phash) = if let (Some(hash), Some(phash)) = (cached_dhash, cached_phash) {
                cache_hits += 1;
                (hash, phash)
            } else {
                let (hash, phash) = match image_hashes_from_path(&canonical) {
                    Ok(hashes) => hashes,
                    Err(_) => {
                        skipped_files += 1;
                        continue;
                    }
                };
                database
                    .save_file_feature(&FileFeature {
                        file_id: file.id.clone(),
                        feature_kind: "image_dhash".to_owned(),
                        model_version: IMAGE_DHASH_VERSION.to_owned(),
                        size: file.size,
                        modified_at_ns: file.modified_at_ns,
                        changed_at_ns: file.changed_at_ns,
                        dimensions: 64,
                        quantization: "binary64".to_owned(),
                        feature_blob: hash.to_le_bytes().to_vec(),
                        created_at_ms: now_ms(),
                    })
                    .map_err(display_error)?;
                database
                    .save_file_feature(&FileFeature {
                        file_id: file.id.clone(),
                        feature_kind: "image_phash".to_owned(),
                        model_version: IMAGE_PHASH_VERSION.to_owned(),
                        size: file.size,
                        modified_at_ns: file.modified_at_ns,
                        changed_at_ns: file.changed_at_ns,
                        dimensions: 64,
                        quantization: "binary64".to_owned(),
                        feature_blob: phash.to_le_bytes().to_vec(),
                        created_at_ms: now_ms(),
                    })
                    .map_err(display_error)?;
                computed_features += 1;
                (hash, phash)
            };
            hashes.push(SimilarityHashInput {
                id: file.id.clone(),
                hash,
            });
            phashes.insert(file.id.clone(), phash);
            files.insert(file.id.clone(), file);
        }
        let Some(next) = page.next_cursor else { break };
        cursor = Some(next);
    }
    let feature_ms = feature_started.elapsed().as_millis() as u64;
    let candidate_started = Instant::now();
    let candidates = find_similarity_candidates(&hashes, 8).map_err(display_error)?;
    let candidate_pairs = candidates.len();
    let pairs = candidates
        .into_iter()
        .filter_map(|candidate| {
            let left = files.get(&candidate.left_id)?;
            let right = files.get(&candidate.right_id)?;
            let phash_distance =
                (phashes.get(&candidate.left_id)? ^ phashes.get(&candidate.right_id)?).count_ones();
            if phash_distance > 12 {
                return None;
            }
            let phash_similarity = 1.0 - phash_distance as f32 / 64.0;
            Some(SimilarContentPairView {
                left_file_id: left.id.clone(),
                left_path: left.current_path.clone(),
                right_file_id: right.id.clone(),
                right_path: right.current_path.clone(),
                hamming_distance: candidate.hamming_distance,
                secondary_distance: Some(phash_distance),
                secondary_similarity: phash_similarity,
                similarity: candidate.similarity * 0.4 + phash_similarity * 0.6,
                verification: "dHash 候选 + DCT pHash 复核".to_owned(),
            })
        })
        .collect::<Vec<_>>();
    let candidate_ms = candidate_started.elapsed().as_millis() as u64;
    let secondary_verified_pairs = pairs.len();
    Ok((
        SimilarContentReportView {
            content_kind: "image".to_owned(),
            input_files: hashes.len(),
            computed_features,
            cache_hits,
            skipped_files,
            candidate_pairs,
            secondary_verified_pairs,
            feature_ms,
            candidate_ms,
            algorithm_profile: "dhash64-mih3-r8+phash64-r12-v1".to_owned(),
            pairs,
        },
        interrupted,
    ))
}

#[tauri::command]
fn start_similarity_analysis(
    app: AppHandle,
    library_id: String,
    content_kind: String,
) -> Result<String, String> {
    let job_kind = match content_kind.as_str() {
        "text" => "similar_text_analysis",
        "image" => "similar_image_analysis",
        _ => return Err("不支持的相似内容类型".to_owned()),
    };
    let timestamp = now_ms();
    let mut job_id = unique_id(job_kind, timestamp);
    {
        let state = app.state::<DesktopState>();
        let mut database = lock_database(&state)?;
        database
            .library_root(&library_id)
            .map_err(display_error)?
            .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
        let payload = serde_json::to_string(&SimilarityPayload {
            library_id,
            content_kind,
        })
        .map_err(display_error)?;
        job_id = database
            .enqueue_or_reuse_job(&NewJob {
                id: job_id.clone(),
                kind: job_kind.to_owned(),
                payload_json: payload,
                priority: 105,
                progress_total: None,
                created_at_ms: timestamp,
            })
            .map_err(display_error)?;
    }
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || run_one_job(worker_app));
    Ok(job_id)
}

#[tauri::command]
fn similarity_result(
    state: State<'_, DesktopState>,
    job_id: String,
) -> Result<Option<SimilarContentReportView>, String> {
    let database = lock_database(&state)?;
    let Some(json) = database.job_result(&job_id).map_err(display_error)? else {
        return Ok(None);
    };
    serde_json::from_str(&json).map(Some).map_err(display_error)
}

#[tauri::command]
fn start_exact_duplicate_analysis(app: AppHandle, library_id: String) -> Result<String, String> {
    let timestamp = now_ms();
    let mut job_id = unique_id("duplicates", timestamp);
    {
        let state = app.state::<DesktopState>();
        let mut database = lock_database(&state)?;
        database
            .library_root(&library_id)
            .map_err(display_error)?
            .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
        let payload =
            serde_json::to_string(&DuplicatePayload { library_id }).map_err(display_error)?;
        job_id = database
            .enqueue_or_reuse_job(&NewJob {
                id: job_id.clone(),
                kind: "exact_duplicate_analysis".to_owned(),
                payload_json: payload,
                priority: 110,
                progress_total: None,
                created_at_ms: timestamp,
            })
            .map_err(display_error)?;
    }
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || run_one_job(worker_app));
    Ok(job_id)
}

#[tauri::command]
fn exact_duplicate_result(
    state: State<'_, DesktopState>,
    job_id: String,
) -> Result<Option<DuplicateReportView>, String> {
    let database = lock_database(&state)?;
    let Some(json) = database.job_result(&job_id).map_err(display_error)? else {
        return Ok(None);
    };
    serde_json::from_str(&json).map(Some).map_err(display_error)
}

fn compute_exact_duplicates(
    app: &AppHandle,
    library_id: &str,
    should_continue: impl FnMut(DuplicateProgress) -> bool,
) -> Result<(DuplicateReportView, bool), String> {
    // P2：使用独立 worker 连接，避免持有主互斥锁阻塞 UI 轮询。
    let worker = open_worker_database(app)?;
    let library = worker
        .library_root(library_id)
        .map_err(display_error)?
        .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
    let root = PathBuf::from(&library.root_path)
        .canonicalize()
        .map_err(display_error)?;
    let mut cursor = None;
    let mut indexed = Vec::new();
    loop {
        let page = worker
            .list_files_page(library_id, cursor.as_ref(), 500)
            .map_err(display_error)?;
        for file in page.items {
            let path = PathBuf::from(&file.current_path);
            if let Ok(canonical) = path.canonicalize() {
                if canonical.starts_with(&root) {
                    let cache = worker
                        .file_hash_cache(library_id, &file, EXACT_HASH_ALGORITHM)
                        .map_err(display_error)?;
                    indexed.push((file, canonical, cache));
                }
            }
        }
        let Some(next) = page.next_cursor else { break };
        cursor = Some(next);
    }
    let inputs = indexed
        .iter()
        .map(|(file, path, cache)| DuplicateInput {
            path: path.clone(),
            snapshot: FileSnapshot {
                size: file.size,
                modified_at_ns: file.modified_at_ns,
                changed_at_ns: file.changed_at_ns,
                created_at_ns: None,
            },
            cached_quick_fingerprint: cache
                .as_ref()
                .and_then(|entry| parse_hash_hex(&entry.quick_fingerprint)),
            cached_content_hash: cache
                .as_ref()
                .and_then(|entry| entry.content_hash.as_deref())
                .and_then(parse_hash_hex),
        })
        .collect::<Vec<_>>();
    let controlled = find_exact_duplicates_cached_controlled(&inputs, should_continue);
    let files_by_path = indexed
        .iter()
        .map(|(file, path, _)| (path.clone(), file))
        .collect::<HashMap<_, _>>();
    if !controlled.result.computed_hashes.is_empty() {
        let timestamp = now_ms();
        let caches: Vec<FileHashCache> = controlled
            .result
            .computed_hashes
            .iter()
            .filter_map(|computed| {
                let file = files_by_path.get(&computed.path)?;
                Some(FileHashCache {
                    file_id: file.id.clone(),
                    size: file.size,
                    modified_at_ns: file.modified_at_ns,
                    changed_at_ns: file.changed_at_ns,
                    algorithm: EXACT_HASH_ALGORITHM.to_owned(),
                    quick_fingerprint: hash_hex(computed.quick_fingerprint),
                    content_hash: computed.content_hash.map(hash_hex),
                    updated_at_ms: timestamp,
                })
            })
            .collect();
        let cache_refs: Vec<&FileHashCache> = caches.iter().collect();
        worker
            .save_file_hash_cache_batch(library_id, &cache_refs)
            .map_err(display_error)?;
    }
    let report = controlled.result.report;
    let view = DuplicateReportView {
        input_files: report.stats.input_files,
        quick_fingerprinted_files: report.stats.quick_fingerprinted_files,
        fully_hashed_files: report.stats.fully_hashed_files,
        quick_cache_hits: report.stats.quick_cache_hits,
        full_cache_hits: report.stats.full_cache_hits,
        skipped_files: report.stats.skipped_files,
        groups: report
            .groups
            .into_iter()
            .map(|group| duplicate_group_view(group, &files_by_path))
            .collect(),
    };
    Ok((view, controlled.interrupted))
}

fn duplicate_group_view(
    group: guixu_analysis::DuplicateGroup,
    files_by_path: &HashMap<PathBuf, &IndexedFile>,
) -> DuplicateGroupView {
    let paths = group
        .paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let ranked = rank_duplicate_retention(
        &group
            .paths
            .iter()
            .filter_map(|path| {
                files_by_path.get(path).map(|file| RetentionInput {
                    path: path.clone(),
                    modified_at_ns: file.modified_at_ns,
                })
            })
            .collect::<Vec<_>>(),
    );
    let suggested_keep = ranked
        .first()
        .map(|recommendation| recommendation.path.to_string_lossy().into_owned())
        .unwrap_or_else(|| paths.first().cloned().unwrap_or_default());
    let files = ranked
        .into_iter()
        .filter_map(|recommendation| {
            let file = files_by_path.get(&recommendation.path)?;
            let path = recommendation.path.to_string_lossy().into_owned();
            Some(DuplicateFileView {
                file_id: file.id.clone(),
                suggested_keep: path == suggested_keep,
                path,
                retention_score: recommendation.score,
                reasons: recommendation.reasons,
            })
        })
        .collect();
    DuplicateGroupView {
        id: hash_hex(group.content_hash),
        size: group.size,
        potential_savings: group
            .size
            .saturating_mul(paths.len().saturating_sub(1) as u64),
        paths,
        suggested_keep,
        files,
    }
}

#[tauri::command]
async fn undo_operation(app: AppHandle, operation_id: String) -> Result<ExecutionView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let timestamp = now_ms();
        let mut database = open_worker_database(&app)?;
        let operation = database
            .operation(&operation_id)
            .map_err(display_error)?
            .ok_or_else(|| "操作记录不存在".to_owned())?;
        let root = database
            .library_root(&operation.library_id)
            .map_err(display_error)?
            .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
        let report = undo_stored_operation(
            &mut database,
            Path::new(&root.root_path),
            &operation_id,
            timestamp,
        )
        .map_err(display_error)?;
        Ok(ExecutionView {
            operation_id: report.operation_id,
            completed_items: report.completed_items,
            total_items: report.total_items,
        })
    })
    .await
    .map_err(display_error)?
}

fn operation_view(operation: StoredOperation) -> OperationView {
    OperationView {
        id: operation.id,
        operation_kind: operation.operation_kind.as_str().to_owned(),
        status: operation.status,
        created_at_ms: operation.created_at_ms,
        completed_at_ms: operation.completed_at_ms,
        error_message: operation.error_message,
        items: operation
            .items
            .into_iter()
            .map(|item| OperationItemView {
                ordinal: item.ordinal,
                status: item.status,
                source_path: item.source_path,
                target_path: item.target_path,
                error_message: item.error_message,
            })
            .collect(),
    }
}

fn category_label(category: FileCategory) -> &'static str {
    match category {
        FileCategory::Document => "文档",
        FileCategory::Image => "图片",
        FileCategory::Audio => "音频",
        FileCategory::Video => "视频",
        FileCategory::Archive => "压缩包",
        FileCategory::Code => "代码",
        FileCategory::Data => "数据",
        FileCategory::Other => "其他",
    }
}

fn run_one_job(app: AppHandle) -> bool {
    let worker_id = format!("desktop-{}", std::process::id());
    let mut database = match open_worker_database(&app) {
        Ok(database) => database,
        Err(_) => return false,
    };
    let claimed = match database.claim_next_job(&worker_id, now_ms(), 24 * 60 * 60 * 1000) {
        Ok(Some(job)) => job,
        _ => return false,
    };
    if claimed.kind == "exact_duplicate_analysis" {
        drop(database);
        run_duplicate_analysis_job(app, &worker_id, claimed);
        return true;
    }
    if matches!(
        claimed.kind.as_str(),
        "similar_text_analysis" | "similar_image_analysis"
    ) {
        drop(database);
        run_similarity_analysis_job(app, &worker_id, claimed);
        return true;
    }
    if !matches!(claimed.kind.as_str(), "library_scan" | "snapshot_reconcile") {
        let detail =
            serde_json::json!({ "message": format!("未知任务类型：{}", claimed.kind) }).to_string();
        let _ = database.fail_job(&claimed.id, &worker_id, now_ms(), Some(&detail));
        return true;
    }
    let payload: ScanPayload = match serde_json::from_str(&claimed.payload_json) {
        Ok(payload) => payload,
        Err(error) => {
            let detail = serde_json::json!({ "message": error.to_string() }).to_string();
            let _ = database.fail_job(&claimed.id, &worker_id, now_ms(), Some(&detail));
            return true;
        }
    };
    let control_database = match open_worker_database(&app) {
        Ok(database) => database,
        Err(error) => {
            let detail = serde_json::json!({ "message": error }).to_string();
            let _ = database.fail_job(&claimed.id, &worker_id, now_ms(), Some(&detail));
            return true;
        }
    };
    let mut last_reported_entries = 0_u64;
    let mut last_reported_at = Instant::now();
    let mut requested_control = None;
    let mut control_error = None;
    match reconcile_snapshot(
        &mut database,
        &payload.library_id,
        Path::new(&payload.root_path),
        now_ms(),
        &ScanOptions::default(),
        |progress| {
            let should_report = progress
                .visited_entries
                .saturating_sub(last_reported_entries)
                >= 128
                || last_reported_at.elapsed() >= Duration::from_millis(500);
            if !should_report {
                return true;
            }
            let timestamp = now_ms();
            if let Err(error) = control_database.update_job_progress(
                &claimed.id,
                &worker_id,
                progress.indexed_files,
                None,
                timestamp,
            ) {
                control_error = Some(error.to_string());
                return false;
            }
            if let Err(error) = control_database.renew_job_lease(
                &claimed.id,
                &worker_id,
                timestamp,
                24 * 60 * 60 * 1000,
            ) {
                control_error = Some(error.to_string());
                return false;
            }
            let _ = app.emit("job-status-changed", &claimed.id);
            match control_database.job_status(&claimed.id) {
                Ok(Some(JobStatus::PauseRequested)) => {
                    requested_control = Some(JobStatus::PauseRequested);
                    false
                }
                Ok(Some(JobStatus::CancelRequested)) => {
                    requested_control = Some(JobStatus::CancelRequested);
                    false
                }
                Ok(Some(JobStatus::Running)) => {
                    last_reported_entries = progress.visited_entries;
                    last_reported_at = Instant::now();
                    true
                }
                Ok(Some(status)) => {
                    control_error = Some(format!("任务扫描时进入了意外状态：{status:?}"));
                    false
                }
                Ok(None) => {
                    control_error = Some("任务记录在扫描过程中消失".to_owned());
                    false
                }
                Err(error) => {
                    control_error = Some(error.to_string());
                    false
                }
            }
        },
    ) {
        Ok(report) => {
            let _ = control_database.update_job_progress(
                &claimed.id,
                &worker_id,
                report.progress.indexed_files,
                None,
                now_ms(),
            );
            if let Some(error) = control_error {
                let detail = serde_json::json!({ "message": error }).to_string();
                let _ = control_database.fail_job(&claimed.id, &worker_id, now_ms(), Some(&detail));
            } else if report.cancelled {
                let requested = requested_control.or_else(|| {
                    control_database
                        .job_status(&claimed.id)
                        .ok()
                        .flatten()
                        .filter(|status| {
                            matches!(
                                status,
                                JobStatus::PauseRequested | JobStatus::CancelRequested
                            )
                        })
                });
                if let Some(requested) = requested {
                    let _ = control_database.acknowledge_job_control(
                        &claimed.id,
                        &worker_id,
                        requested,
                        now_ms(),
                    );
                }
            } else {
                match control_database.job_status(&claimed.id) {
                    Ok(Some(requested @ JobStatus::PauseRequested))
                    | Ok(Some(requested @ JobStatus::CancelRequested)) => {
                        let _ = control_database.acknowledge_job_control(
                            &claimed.id,
                            &worker_id,
                            requested,
                            now_ms(),
                        );
                    }
                    _ => {
                        finish_job(
                            &control_database,
                            &app,
                            &claimed.id,
                            &worker_id,
                            control_database.complete_job(&claimed.id, &worker_id, now_ms()),
                            now_ms(),
                        );
                    }
                }
            }
            let _ = app.emit("job-status-changed", &claimed.id);
        }
        Err(error) => {
            let detail = serde_json::json!({ "message": error.to_string() }).to_string();
            let _ = control_database.fail_job(&claimed.id, &worker_id, now_ms(), Some(&detail));
            let _ = app.emit("job-status-changed", &claimed.id);
        }
    }
    true
}

fn fail_claimed_job(app: &AppHandle, job_id: &str, worker_id: &str, message: &str) {
    let detail = serde_json::json!({ "message": message }).to_string();
    if let Ok(database) = open_worker_database(app) {
        let _ = database.fail_job(job_id, worker_id, now_ms(), Some(&detail));
    } else {
        let state = app.state::<DesktopState>();
        if let Ok(database) = state.database.lock() {
            let _ = database.fail_job(job_id, worker_id, now_ms(), Some(&detail));
        }
    }
    let _ = app.emit("job-status-changed", job_id);
}

fn run_duplicate_analysis_job(app: AppHandle, worker_id: &str, claimed: ClaimedJob) {
    let payload: DuplicatePayload = match serde_json::from_str(&claimed.payload_json) {
        Ok(payload) => payload,
        Err(error) => {
            fail_claimed_job(&app, &claimed.id, worker_id, &error.to_string());
            return;
        }
    };
    let control_database = match open_worker_database(&app) {
        Ok(database) => database,
        Err(error) => {
            fail_claimed_job(&app, &claimed.id, worker_id, &error);
            return;
        }
    };
    let mut requested_control = None;
    let mut control_error = None;
    let result = compute_exact_duplicates(&app, &payload.library_id, |progress| {
        let timestamp = now_ms();
        let current = u64::try_from(progress.processed_files).unwrap_or(u64::MAX);
        let total = u64::try_from(progress.total_work).unwrap_or(u64::MAX);
        if let Err(error) = control_database.update_job_progress(
            &claimed.id,
            worker_id,
            current,
            Some(total),
            timestamp,
        ) {
            control_error = Some(error.to_string());
            return false;
        }
        if let Err(error) =
            control_database.renew_job_lease(&claimed.id, worker_id, timestamp, 24 * 60 * 60 * 1000)
        {
            control_error = Some(error.to_string());
            return false;
        }
        let _ = app.emit("job-status-changed", &claimed.id);
        match control_database.job_status(&claimed.id) {
            Ok(Some(JobStatus::PauseRequested)) => {
                requested_control = Some(JobStatus::PauseRequested);
                false
            }
            Ok(Some(JobStatus::CancelRequested)) => {
                requested_control = Some(JobStatus::CancelRequested);
                false
            }
            Ok(Some(JobStatus::Running)) => true,
            Ok(Some(status)) => {
                control_error = Some(format!("重复分析进入了意外状态：{status:?}"));
                false
            }
            Ok(None) => {
                control_error = Some("重复分析任务记录消失".to_owned());
                false
            }
            Err(error) => {
                control_error = Some(error.to_string());
                false
            }
        }
    });
    match result {
        Ok((report, interrupted)) => {
            if let Some(error) = control_error {
                let detail = serde_json::json!({ "message": error }).to_string();
                let _ = control_database.fail_job(&claimed.id, worker_id, now_ms(), Some(&detail));
            } else if interrupted {
                let requested = requested_control.or_else(|| {
                    control_database
                        .job_status(&claimed.id)
                        .ok()
                        .flatten()
                        .filter(|status| {
                            matches!(
                                status,
                                JobStatus::PauseRequested | JobStatus::CancelRequested
                            )
                        })
                });
                if let Some(requested) = requested {
                    let _ = control_database.acknowledge_job_control(
                        &claimed.id,
                        worker_id,
                        requested,
                        now_ms(),
                    );
                }
            } else {
                let json = match serde_json::to_string(&report) {
                    Ok(json) => json,
                    Err(error) => {
                        let detail =
                            serde_json::json!({ "message": error.to_string() }).to_string();
                        let _ = control_database.fail_job(
                            &claimed.id,
                            worker_id,
                            now_ms(),
                            Some(&detail),
                        );
                        let _ = app.emit("job-status-changed", &claimed.id);
                        return;
                    }
                };
                finish_job(
                    &control_database,
                    &app,
                    &claimed.id,
                    worker_id,
                    control_database.complete_job_with_result(
                        &claimed.id,
                        worker_id,
                        &json,
                        now_ms(),
                    ),
                    now_ms(),
                );
            }
        }
        Err(error) => {
            let detail = serde_json::json!({ "message": error }).to_string();
            let _ = control_database.fail_job(&claimed.id, worker_id, now_ms(), Some(&detail));
        }
    }
    let _ = app.emit("job-status-changed", &claimed.id);
}

fn run_similarity_analysis_job(app: AppHandle, worker_id: &str, claimed: ClaimedJob) {
    let payload: SimilarityPayload = match serde_json::from_str(&claimed.payload_json) {
        Ok(payload) => payload,
        Err(error) => {
            fail_claimed_job(&app, &claimed.id, worker_id, &error.to_string());
            return;
        }
    };
    let control_database = match open_worker_database(&app) {
        Ok(database) => database,
        Err(error) => {
            fail_claimed_job(&app, &claimed.id, worker_id, &error);
            return;
        }
    };
    let mut requested_control = None;
    let mut control_error = None;
    let mut last_reported = 0_u64;
    let mut last_reported_at = Instant::now();
    let result = {
        let mut control = |current: u64, total: u64| {
            let should_report = current == total
                || current.saturating_sub(last_reported) >= 16
                || last_reported_at.elapsed() >= Duration::from_millis(250);
            if !should_report {
                return true;
            }
            let timestamp = now_ms();
            if let Err(error) = control_database.update_job_progress(
                &claimed.id,
                worker_id,
                current,
                Some(total),
                timestamp,
            ) {
                control_error = Some(error.to_string());
                return false;
            }
            if let Err(error) = control_database.renew_job_lease(
                &claimed.id,
                worker_id,
                timestamp,
                24 * 60 * 60 * 1000,
            ) {
                control_error = Some(error.to_string());
                return false;
            }
            last_reported = current;
            last_reported_at = Instant::now();
            let _ = app.emit("job-status-changed", &claimed.id);
            match control_database.job_status(&claimed.id) {
                Ok(Some(JobStatus::PauseRequested)) => {
                    requested_control = Some(JobStatus::PauseRequested);
                    false
                }
                Ok(Some(JobStatus::CancelRequested)) => {
                    requested_control = Some(JobStatus::CancelRequested);
                    false
                }
                Ok(Some(JobStatus::Running)) => true,
                Ok(Some(status)) => {
                    control_error = Some(format!("相似内容分析进入了意外状态：{status:?}"));
                    false
                }
                Ok(None) => {
                    control_error = Some("相似内容分析任务记录消失".to_owned());
                    false
                }
                Err(error) => {
                    control_error = Some(error.to_string());
                    false
                }
            }
        };
        match payload.content_kind.as_str() {
            "text" => compute_similar_texts(&app, &payload.library_id, &mut control),
            "image" => compute_similar_images(&app, &payload.library_id, &mut control),
            _ => Err("不支持的相似内容任务类型".to_owned()),
        }
    };
    match result {
        Ok((report, interrupted)) => {
            if let Some(error) = control_error {
                let detail = serde_json::json!({ "message": error }).to_string();
                let _ = control_database.fail_job(&claimed.id, worker_id, now_ms(), Some(&detail));
            } else if interrupted {
                let requested = requested_control.or_else(|| {
                    control_database
                        .job_status(&claimed.id)
                        .ok()
                        .flatten()
                        .filter(|status| {
                            matches!(
                                status,
                                JobStatus::PauseRequested | JobStatus::CancelRequested
                            )
                        })
                });
                if let Some(requested) = requested {
                    let _ = control_database.acknowledge_job_control(
                        &claimed.id,
                        worker_id,
                        requested,
                        now_ms(),
                    );
                }
            } else {
                match serde_json::to_string(&report) {
                    Ok(json) => finish_job(
                        &control_database,
                        &app,
                        &claimed.id,
                        worker_id,
                        control_database.complete_job_with_result(
                            &claimed.id,
                            worker_id,
                            &json,
                            now_ms(),
                        ),
                        now_ms(),
                    ),
                    Err(error) => {
                        let detail =
                            serde_json::json!({ "message": error.to_string() }).to_string();
                        let _ = control_database.fail_job(
                            &claimed.id,
                            worker_id,
                            now_ms(),
                            Some(&detail),
                        );
                    }
                }
            }
        }
        Err(error) => {
            let detail = serde_json::json!({ "message": error }).to_string();
            let _ = control_database.fail_job(&claimed.id, worker_id, now_ms(), Some(&detail));
        }
    }
    let _ = app.emit("job-status-changed", &claimed.id);
}

fn schedule_watch_reconciliation(app: &AppHandle) -> Result<(), String> {
    let requests = {
        let state = app.state::<DesktopState>();
        let mut watchers = state
            .watchers
            .lock()
            .map_err(|_| "文件事件监听器锁已损坏".to_owned())?;
        watchers
            .iter_mut()
            .filter_map(|(library_id, watcher)| {
                watcher.poll().map(|request| (library_id.clone(), request))
            })
            .collect::<Vec<_>>()
    };
    if requests.is_empty() {
        return Ok(());
    }
    let state = app.state::<DesktopState>();
    let mut database = lock_database(&state)?;
    for (library_id, request) in requests {
        let root = database
            .library_root(&library_id)
            .map_err(display_error)?
            .ok_or_else(|| "监听资料库不存在".to_owned())?;
        let directories = match request {
            ReconcileRequest::FullSnapshot => vec![PathBuf::from(root.root_path)],
            ReconcileRequest::Directories(directories) => directories,
        };
        for directory in directories {
            if directory
                .components()
                .any(|component| component.as_os_str() == ".guixu-trash")
            {
                continue;
            }
            let Some(root_path) = directory.to_str() else {
                continue;
            };
            let timestamp = now_ms();
            let id = unique_id("reconcile", timestamp);
            let payload = serde_json::to_string(&ScanPayload {
                library_id: library_id.clone(),
                root_path: root_path.to_owned(),
            })
            .map_err(display_error)?;
            database
                .enqueue_or_reuse_job(&NewJob {
                    id,
                    kind: "snapshot_reconcile".to_owned(),
                    payload_json: payload,
                    priority: 80,
                    progress_total: None,
                    created_at_ms: timestamp,
                })
                .map_err(display_error)?;
            let worker_app = app.clone();
            tauri::async_runtime::spawn_blocking(move || run_one_job(worker_app));
        }
    }
    Ok(())
}

fn lock_database<'a>(
    state: &'a State<'_, DesktopState>,
) -> Result<std::sync::MutexGuard<'a, Database>, String> {
    state
        .database
        .lock()
        .map_err(|_| "本地数据库锁已损坏".to_owned())
}

fn open_worker_database(app: &AppHandle) -> Result<Database, String> {
    let path = app.state::<DesktopState>().database_path.clone();
    Database::open(path).map_err(display_error)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn hash_hex(hash: [u8; 32]) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_hash_hex(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut hash = [0_u8; 32];
    for (index, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(hash)
}

fn unique_id(prefix: &str, timestamp: i64) -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{timestamp}-{}-{sequence}", std::process::id())
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

use guixu_storage::JobTransferResult;

/// H6 修复：安全结束任务——若状态转移因竞态（任务已被中断）而失败，自动确认中断请求。
fn finish_job(
    db: &Database,
    app: &AppHandle,
    job_id: &str,
    worker_id: &str,
    outcome: Result<JobTransferResult, guixu_storage::StorageError>,
    now_ms: i64,
) {
    match outcome {
        Ok(transfer) if transfer.applied => {
            let _ = app.emit("job-status-changed", job_id);
        }
        Ok(transfer) => {
            if let Some(ref status) = transfer.current_status {
                match status.as_str() {
                    "cancel_requested" => {
                        let _ = db
                            .acknowledge_job_control(
                                job_id,
                                worker_id,
                                JobStatus::CancelRequested,
                                now_ms,
                            )
                            .ok();
                    }
                    "pause_requested" => {
                        let _ = db
                            .acknowledge_job_control(
                                job_id,
                                worker_id,
                                JobStatus::PauseRequested,
                                now_ms,
                            )
                            .ok();
                    }
                    _ => {}
                }
            }
            let _ = app.emit("job-status-changed", job_id);
        }
        Err(error) => {
            let detail = serde_json::json!({ "message": error.to_string() }).to_string();
            let _ = db.fail_job(job_id, worker_id, now_ms, Some(&detail));
            let _ = app.emit("job-status-changed", job_id);
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());
    #[cfg(feature = "e2e")]
    let builder = builder.plugin(tauri_plugin_wdio_webdriver::init());

    builder
        .setup(|app| {
            let data_directory = std::env::var_os("GUIXU_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or(app.path().app_data_dir()?);
            fs::create_dir_all(&data_directory)?;
            let database_path = data_directory.join("guixu.sqlite3");
            let mut database = Database::open(&database_path)
                .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;
            audit_incomplete_operations(&mut database, now_ms())
                .map_err(|error| -> Box<dyn std::error::Error> { Box::new(error) })?;
            // P5：清扫崩溃残留的 .guixu-*.tmp 临时文件。
            if let Some(root) = database.latest_library_root().ok().flatten() {
                if let Ok(entries) = std::fs::read_dir(Path::new(&root.root_path)) {
                    for entry in entries.flatten() {
                        let name = entry.file_name();
                        let name_str = name.to_string_lossy();
                        if name_str.starts_with(".guixu-") && name_str.ends_with(".tmp") {
                            let _ = std::fs::remove_file(entry.path());
                        }
                    }
                }
            }
            let mut watchers = HashMap::new();
            if let Some(root) = database.latest_library_root()? {
                if let Ok(watcher) = PlatformWatcher::start(Path::new(&root.root_path)) {
                    watchers.insert(root.library_id, watcher);
                }
            }
            app.manage(DesktopState {
                database: Mutex::new(database),
                database_path,
                watchers: Mutex::new(watchers),
            });
            // P1 启动恢复：消费崩溃遗留的 queued 任务。
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn_blocking(move || while run_one_job(handle.clone()) {});
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            runtime_info,
            app_settings,
            save_app_settings,
            local_data_status,
            clear_hash_cache,
            active_library,
            select_library_folder,
            rescan_library,
            files_page,
            library_overview,
            preview_file,
            open_file,
            reveal_file,
            jobs,
            poll_file_events,
            pause_job,
            resume_job,
            cancel_job,
            search_files,
            smart_folders,
            save_smart_folder,
            delete_smart_folder,
            create_organize_plan,
            create_rename_plan,
            create_copy_plan,
            create_move_plan,
            create_trash_plan,
            execute_organize_plan,
            execute_rename_plan,
            execute_copy_plan,
            execute_move_plan,
            execute_trash_plan,
            operation_history,
            undo_operation,
            start_similarity_analysis,
            similarity_result,
            start_exact_duplicate_analysis,
            exact_duplicate_result
        ])
        .run(tauri::generate_context!())
        .expect("归序桌面程序启动失败");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorized_path_accepts_regular_files_only_inside_the_library() {
        let library = tempfile::tempdir().expect("library directory");
        let outside = tempfile::tempdir().expect("outside directory");
        let inside_file = library.path().join("inside.txt");
        let outside_file = outside.path().join("outside.txt");
        fs::write(&inside_file, b"inside").expect("write inside file");
        fs::write(&outside_file, b"outside").expect("write outside file");

        assert_eq!(
            validate_authorized_path(library.path(), &inside_file).expect("inside path"),
            inside_file.canonicalize().expect("canonical inside")
        );
        assert!(validate_authorized_path(library.path(), &outside_file).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn authorized_path_rejects_symbolic_links_even_when_the_target_is_inside() {
        let library = tempfile::tempdir().expect("library directory");
        let target = library.path().join("target.txt");
        let link = library.path().join("link.txt");
        fs::write(&target, b"target").expect("write target");
        std::os::unix::fs::symlink(&target, &link).expect("create link");
        assert!(validate_authorized_path(library.path(), &link).is_err());
    }

    #[test]
    fn plan_execution_commands_enforce_the_expected_operation_kind() {
        assert!(ensure_plan_kind(FileOperationKind::Rename, FileOperationKind::Rename).is_ok());
        assert!(ensure_plan_kind(FileOperationKind::Rename, FileOperationKind::Organize).is_err());
        assert!(ensure_plan_kind(FileOperationKind::Move, FileOperationKind::Move).is_ok());
        assert!(ensure_plan_kind(FileOperationKind::Copy, FileOperationKind::Move).is_err());
    }

    #[test]
    fn app_settings_keep_backward_compatible_defaults() {
        let settings: AppSettingsView = serde_json::from_str(
            r#"{"defaultViewMode":"grid","pageSize":50,"refreshIntervalMs":5000}"#,
        )
        .expect("deserialize earlier settings payload");

        assert!(settings.restore_last_library);
        assert_eq!(settings.default_view_mode, "grid");
        assert_eq!(settings.default_sort_key, "name");
        assert_eq!(settings.default_sort_direction, "asc");
        assert_eq!(settings.page_size, 50);
    }
}
