use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use guixu_analysis::{FileCategory, classify_file, find_exact_duplicates, unique_filename};
use guixu_domain::{JobStatus, RuntimeInfo};
use guixu_indexer::{PlatformWatcher, ReconcileRequest, ScanOptions, reconcile_snapshot};
use guixu_operations::{audit_incomplete_operations, execute_stored_plan, undo_stored_operation};
use guixu_platform::{FileLaunchAction, launch_file, observe_file};
use guixu_storage::{
    Database, FilePageCursor, IndexedFile, NewJob, NewPlanItem, StoredOperation, parse_search_query,
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
struct ScanPayload {
    library_id: String,
    root_path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatePlanRequest {
    library_id: String,
    file_ids: Vec<String>,
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
    status: String,
    created_at_ms: i64,
    completed_at_ms: Option<i64>,
    error_message: Option<String>,
    items: Vec<OperationItemView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateGroupView {
    id: String,
    size: u64,
    potential_savings: u64,
    paths: Vec<String>,
    suggested_keep: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateReportView {
    input_files: usize,
    quick_fingerprinted_files: usize,
    fully_hashed_files: usize,
    skipped_files: usize,
    groups: Vec<DuplicateGroupView>,
}

#[tauri::command]
fn runtime_info() -> RuntimeInfo {
    RuntimeInfo::current()
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
    let record = {
        let state = app.state::<DesktopState>();
        let database = lock_database(&state)?;
        let record = database
            .register_library_root(&library_id, &root_id, &name, &root_path, timestamp)
            .map_err(display_error)?;
        let payload = serde_json::to_string(&ScanPayload {
            library_id: record.library_id.clone(),
            root_path: record.root_path.clone(),
        })
        .map_err(display_error)?;
        database
            .enqueue_job(&NewJob {
                id: scan_job_id.clone(),
                kind: "library_scan".to_owned(),
                payload_json: payload,
                priority: 100,
                progress_total: None,
                created_at_ms: timestamp,
            })
            .map_err(display_error)?;
        record
    };
    {
        let state = app.state::<DesktopState>();
        let watcher =
            PlatformWatcher::start(Path::new(&record.root_path)).map_err(display_error)?;
        state
            .watchers
            .lock()
            .map_err(|_| "文件事件监听器锁已损坏".to_owned())?
            .insert(record.library_id.clone(), watcher);
    }
    let worker_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || run_one_scan_job(worker_app));
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
    let job_id = unique_id("scan", timestamp);
    {
        let state = app.state::<DesktopState>();
        let database = lock_database(&state)?;
        let root = database
            .library_root(&library_id)
            .map_err(display_error)?
            .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
        let payload = serde_json::to_string(&ScanPayload {
            library_id,
            root_path: root.root_path,
        })
        .map_err(display_error)?;
        database
            .enqueue_job(&NewJob {
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
    tauri::async_runtime::spawn_blocking(move || run_one_scan_job(worker_app));
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
            request.limit.unwrap_or(100),
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
    tauri::async_runtime::spawn_blocking(move || run_one_scan_job(worker_app));
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
        .create_plan(
            &plan_id,
            &request.library_id,
            timestamp,
            expires_at_ms,
            &stored_items,
        )
        .map_err(display_error)?;
    Ok(PlanView {
        id: plan_id,
        expires_at_ms,
        items: view_items,
    })
}

#[tauri::command]
async fn execute_organize_plan(app: AppHandle, plan_id: String) -> Result<ExecutionView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let timestamp = now_ms();
        let operation_id = unique_id("operation", timestamp);
        let mut database = open_worker_database(&app)?;
        let plan = database
            .plan(&plan_id)
            .map_err(display_error)?
            .ok_or_else(|| "整理计划不存在".to_owned())?;
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

#[tauri::command]
async fn exact_duplicates(
    app: AppHandle,
    library_id: String,
) -> Result<DuplicateReportView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<DesktopState>();
        let database = lock_database(&state)?;
        let library = database
            .library_root(&library_id)
            .map_err(display_error)?
            .ok_or_else(|| "资料库不存在或未授权".to_owned())?;
        let root = PathBuf::from(&library.root_path)
            .canonicalize()
            .map_err(display_error)?;
        let mut cursor = None;
        let mut paths = Vec::new();
        loop {
            let page = database
                .list_files_page(&library_id, cursor.as_ref(), 500)
                .map_err(display_error)?;
            for file in page.items {
                let path = PathBuf::from(file.current_path);
                if let Ok(canonical) = path.canonicalize() {
                    if canonical.starts_with(&root) {
                        paths.push(canonical);
                    }
                }
            }
            let Some(next) = page.next_cursor else { break };
            cursor = Some(next);
        }
        drop(database);
        let report = find_exact_duplicates(&paths);
        Ok(DuplicateReportView {
            input_files: report.stats.input_files,
            quick_fingerprinted_files: report.stats.quick_fingerprinted_files,
            fully_hashed_files: report.stats.fully_hashed_files,
            skipped_files: report.stats.skipped_files,
            groups: report
                .groups
                .into_iter()
                .map(|group| {
                    let paths = group
                        .paths
                        .into_iter()
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect::<Vec<_>>();
                    let suggested_keep = paths.first().cloned().unwrap_or_default();
                    DuplicateGroupView {
                        id: group
                            .content_hash
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect(),
                        size: group.size,
                        potential_savings: group
                            .size
                            .saturating_mul(paths.len().saturating_sub(1) as u64),
                        paths,
                        suggested_keep,
                    }
                })
                .collect(),
        })
    })
    .await
    .map_err(display_error)?
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

fn run_one_scan_job(app: AppHandle) {
    let worker_id = format!("desktop-{}", std::process::id());
    let mut database = match open_worker_database(&app) {
        Ok(database) => database,
        Err(_) => return,
    };
    let claimed = match database.claim_next_job(&worker_id, now_ms(), 24 * 60 * 60 * 1000) {
        Ok(Some(job)) => job,
        _ => return,
    };
    let payload: ScanPayload = match serde_json::from_str(&claimed.payload_json) {
        Ok(payload) => payload,
        Err(error) => {
            let detail = serde_json::json!({ "message": error.to_string() }).to_string();
            let _ = database.fail_job(&claimed.id, &worker_id, now_ms(), Some(&detail));
            return;
        }
    };
    let control_database = match open_worker_database(&app) {
        Ok(database) => database,
        Err(error) => {
            let detail = serde_json::json!({ "message": error }).to_string();
            let _ = database.fail_job(&claimed.id, &worker_id, now_ms(), Some(&detail));
            return;
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
                        let _ = control_database.complete_job(&claimed.id, &worker_id, now_ms());
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
    let database = lock_database(&state)?;
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
                .enqueue_job(&NewJob {
                    id,
                    kind: "snapshot_reconcile".to_owned(),
                    payload_json: payload,
                    priority: 80,
                    progress_total: None,
                    created_at_ms: timestamp,
                })
                .map_err(display_error)?;
            let worker_app = app.clone();
            tauri::async_runtime::spawn_blocking(move || run_one_scan_job(worker_app));
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

fn unique_id(prefix: &str, timestamp: i64) -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{timestamp}-{}-{sequence}", std::process::id())
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            runtime_info,
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
            execute_organize_plan,
            operation_history,
            undo_operation,
            exact_duplicates
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
}
