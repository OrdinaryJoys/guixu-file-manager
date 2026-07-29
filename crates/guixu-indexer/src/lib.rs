//! 流式文件索引器：目录项只在遍历时短暂驻留内存，并按小批次提交 SQLite。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use guixu_domain::FileIdentity;
use guixu_platform::observe_file;
use guixu_storage::{Database, FileObservation, ReconcileKind, StorageError};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub batch_size: usize,
    pub max_depth: Option<usize>,
    pub excluded_directory_names: HashSet<String>,
    pub max_reported_issues: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            batch_size: 256,
            max_depth: None,
            excluded_directory_names: HashSet::new(),
            max_reported_issues: 100,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanProgress {
    pub visited_entries: u64,
    pub indexed_files: u64,
    pub inserted_files: u64,
    pub updated_files: u64,
    pub skipped_entries: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanIssue {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReport {
    pub progress: ScanProgress,
    pub issues: Vec<ScanIssue>,
    pub cancelled: bool,
    pub incomplete: bool,
}

#[derive(Debug, Error)]
pub enum ScanError {
    #[error("索引根目录无效：{0}")]
    InvalidRoot(PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileRequest {
    FullSnapshot,
    Directories(Vec<PathBuf>),
}

/// 合并高频事件，只保留最小的不重叠脏目录集合。
#[derive(Debug)]
pub struct DirtyDirectorySet {
    root: PathBuf,
    directories: Vec<PathBuf>,
    full_snapshot: bool,
}

impl DirtyDirectorySet {
    pub fn new(root: &Path) -> Result<Self, std::io::Error> {
        Ok(Self {
            root: root.canonicalize()?,
            directories: Vec::new(),
            full_snapshot: false,
        })
    }

    pub fn mark_event_path(&mut self, path: &Path) {
        let directory = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        let normalized = directory
            .canonicalize()
            .unwrap_or_else(|_| directory.to_owned());
        let directory = normalized.as_path();
        if !directory.starts_with(&self.root)
            && !directory.starts_with(self.root.parent().unwrap_or(&self.root))
        {
            return;
        }
        if directory == self.root {
            self.directories.clear();
            self.directories.push(self.root.clone());
            return;
        }
        if self
            .directories
            .iter()
            .any(|existing| directory.starts_with(existing))
        {
            return;
        }
        self.directories
            .retain(|existing| !existing.starts_with(directory));
        self.directories.push(directory.to_owned());
        self.directories.sort();
    }

    pub fn require_full_snapshot(&mut self) {
        self.full_snapshot = true;
        self.directories.clear();
    }

    pub fn drain(&mut self) -> Option<ReconcileRequest> {
        if self.full_snapshot {
            self.full_snapshot = false;
            return Some(ReconcileRequest::FullSnapshot);
        }
        if self.directories.is_empty() {
            None
        } else {
            Some(ReconcileRequest::Directories(std::mem::take(
                &mut self.directories,
            )))
        }
    }
}

enum WatchMessage {
    Paths(Vec<PathBuf>),
    BackendError,
}

pub struct PlatformWatcher {
    _watcher: RecommendedWatcher,
    receiver: Receiver<WatchMessage>,
    dirty: DirtyDirectorySet,
}

impl PlatformWatcher {
    pub fn start(root: &Path) -> Result<Self, ScanError> {
        let root = root.canonicalize()?;
        let (sender, receiver) = mpsc::channel();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let message = match event {
                    Ok(event) => WatchMessage::Paths(event.paths),
                    Err(_) => WatchMessage::BackendError,
                };
                let _ = sender.send(message);
            })
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Self {
            _watcher: watcher,
            receiver,
            dirty: DirtyDirectorySet::new(&root)?,
        })
    }

    /// 非阻塞吸收当前事件批次；事件后端报错时强制退化为完整快照。
    pub fn poll(&mut self) -> Option<ReconcileRequest> {
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                WatchMessage::Paths(paths) => {
                    for path in paths {
                        self.dirty.mark_event_path(&path);
                    }
                }
                WatchMessage::BackendError => self.dirty.require_full_snapshot(),
            }
        }
        self.dirty.drain()
    }
}

/// 扫描一个已授权根目录。`should_continue` 是协作式取消点，也可用来推送进度。
pub fn scan_library<F>(
    database: &mut Database,
    library_id: &str,
    root: &Path,
    observed_at_ms: i64,
    options: &ScanOptions,
    mut should_continue: F,
) -> Result<ScanReport, ScanError>
where
    F: FnMut(&ScanProgress) -> bool,
{
    let root = root.canonicalize()?;
    if !root.is_dir() {
        return Err(ScanError::InvalidRoot(root));
    }
    let batch_size = options.batch_size.clamp(1, 2048);
    let mut stack = vec![(fs::read_dir(&root)?, root.clone(), 0_usize)];
    let mut batch = Vec::with_capacity(batch_size);
    let mut progress = ScanProgress::default();
    let mut issues = Vec::new();
    let mut cancelled = false;
    let mut seen_identities: HashSet<(String, String)> = HashSet::new();

    while !stack.is_empty() {
        if !should_continue(&progress) {
            cancelled = true;
            break;
        }
        let next = stack.last_mut().expect("stack is non-empty").0.next();
        let Some(entry) = next else {
            stack.pop();
            continue;
        };
        progress.visited_entries += 1;
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                progress.skipped_entries += 1;
                push_issue(
                    &mut issues,
                    options.max_reported_issues,
                    &stack.last().expect("dir frame").1,
                    error.to_string(),
                );
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                progress.skipped_entries += 1;
                push_issue(
                    &mut issues,
                    options.max_reported_issues,
                    &path,
                    error.to_string(),
                );
                continue;
            }
        };
        if file_type.is_symlink() {
            progress.skipped_entries += 1;
            continue;
        }
        if file_type.is_dir() {
            let current_depth = stack.last().expect("dir frame").2;
            let name = entry.file_name();
            if options
                .excluded_directory_names
                .contains(name.to_string_lossy().as_ref())
                || options
                    .max_depth
                    .is_some_and(|maximum| current_depth >= maximum)
            {
                progress.skipped_entries += 1;
                continue;
            }
            match fs::read_dir(&path) {
                Ok(directory) => stack.push((directory, path, current_depth + 1)),
                Err(error) => {
                    progress.skipped_entries += 1;
                    push_issue(
                        &mut issues,
                        options.max_reported_issues,
                        &path,
                        error.to_string(),
                    );
                }
            }
            continue;
        }
        if !file_type.is_file() {
            progress.skipped_entries += 1;
            continue;
        }
        let Some(path_text) = path.to_str() else {
            progress.skipped_entries += 1;
            push_issue(
                &mut issues,
                options.max_reported_issues,
                &path,
                "文件名不是有效 UTF-8，当前版本不会以有损路径索引".to_owned(),
            );
            continue;
        };
        match observe_file(&path) {
            Ok(observed) => {
                if !seen_identities.insert((
                    observed.identity.volume_id.clone(),
                    observed.identity.native_file_id.clone(),
                )) {
                    progress.skipped_entries += 1;
                    continue;
                }
                batch.push(FileObservation {
                    new_file_id: stable_record_id(library_id, &observed.identity),
                    path: path_text.to_owned(),
                    identity: observed.identity,
                    snapshot: observed.snapshot,
                });
            }
            Err(error) => {
                progress.skipped_entries += 1;
                push_issue(
                    &mut issues,
                    options.max_reported_issues,
                    &path,
                    error.to_string(),
                );
            }
        }
        if batch.len() >= batch_size {
            flush_batch(
                database,
                library_id,
                observed_at_ms,
                &mut batch,
                &mut progress,
            )?;
        }
    }
    flush_batch(
        database,
        library_id,
        observed_at_ms,
        &mut batch,
        &mut progress,
    )?;
    let incomplete = !issues.is_empty();
    Ok(ScanReport {
        progress,
        issues,
        cancelled,
        incomplete,
    })
}

/// 执行权威快照对账；只有完整扫描未取消时才标记消失文件。
pub fn reconcile_snapshot<F>(
    database: &mut Database,
    library_id: &str,
    directory: &Path,
    observed_at_ms: i64,
    options: &ScanOptions,
    should_continue: F,
) -> Result<ScanReport, ScanError>
where
    F: FnMut(&ScanProgress) -> bool,
{
    let canonical = match directory.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ScanReport {
                progress: ScanProgress::default(),
                issues: vec![],
                cancelled: false,
                incomplete: false,
            });
        }
        Err(error) => return Err(ScanError::Io(error)),
    };
    let report = scan_library(
        database,
        library_id,
        &canonical,
        observed_at_ms,
        options,
        should_continue,
    )?;
    if !report.cancelled && !report.incomplete {
        let directory_text = canonical
            .to_str()
            .ok_or_else(|| ScanError::InvalidRoot(canonical.clone()))?;
        database.mark_unseen_missing(library_id, directory_text, observed_at_ms)?;
    }
    Ok(report)
}

fn flush_batch(
    database: &mut Database,
    library_id: &str,
    observed_at_ms: i64,
    batch: &mut Vec<FileObservation>,
    progress: &mut ScanProgress,
) -> Result<(), StorageError> {
    if batch.is_empty() {
        return Ok(());
    }
    let results = database.reconcile_files_batch(library_id, batch, observed_at_ms)?;
    progress.indexed_files += results.len() as u64;
    for result in results {
        match result.kind {
            ReconcileKind::Inserted => progress.inserted_files += 1,
            ReconcileKind::Moved | ReconcileKind::MetadataUpdated => progress.updated_files += 1,
            ReconcileKind::Unchanged => {}
        }
    }
    batch.clear();
    Ok(())
}

fn stable_record_id(library_id: &str, identity: &FileIdentity) -> String {
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

fn push_issue(issues: &mut Vec<ScanIssue>, maximum: usize, path: &Path, message: String) {
    if issues.len() < maximum {
        issues.push(ScanIssue {
            path: path.to_owned(),
            message,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database(path: &Path) -> Database {
        let database = Database::open(path).expect("open database");
        database
            .connection()
            .execute(
                "INSERT INTO libraries(id,name,created_at_ms,updated_at_ms) VALUES('library','测试',1,1)",
                [],
            )
            .expect("insert library");
        database
    }

    #[test]
    fn streams_nested_files_in_small_batches() {
        let directory = tempfile::tempdir().expect("temp directory");
        let root = directory.path().join("root");
        fs::create_dir_all(root.join("nested")).expect("directories");
        fs::write(root.join("a.txt"), b"a").expect("file a");
        fs::write(root.join("nested/b.txt"), b"b").expect("file b");
        fs::write(root.join("nested/c.txt"), b"c").expect("file c");
        let mut database = database(&directory.path().join("index.sqlite3"));
        let report = scan_library(
            &mut database,
            "library",
            &root,
            10,
            &ScanOptions {
                batch_size: 1,
                ..ScanOptions::default()
            },
            |_| true,
        )
        .expect("scan");
        assert_eq!(report.progress.indexed_files, 3);
        assert_eq!(report.progress.inserted_files, 3);
        assert!(!report.cancelled);
        let page = database.list_files_page("library", None, 10).expect("page");
        assert_eq!(page.items.len(), 3);
    }

    #[test]
    fn cancellation_flushes_already_observed_files() {
        let directory = tempfile::tempdir().expect("temp directory");
        let root = directory.path().join("root");
        fs::create_dir(&root).expect("root");
        for index in 0..10 {
            fs::write(root.join(format!("{index}.txt")), b"data").expect("file");
        }
        let mut database = database(&directory.path().join("index.sqlite3"));
        let report = scan_library(
            &mut database,
            "library",
            &root,
            10,
            &ScanOptions::default(),
            |progress| progress.visited_entries < 3,
        )
        .expect("scan");
        assert!(report.cancelled);
        assert_eq!(report.progress.indexed_files, 3);
    }

    #[cfg(unix)]
    #[test]
    fn never_follows_symbolic_linked_directories() {
        let directory = tempfile::tempdir().expect("temp directory");
        let root = directory.path().join("root");
        let outside = directory.path().join("outside");
        fs::create_dir(&root).expect("root");
        fs::create_dir(&outside).expect("outside");
        fs::write(outside.join("secret.txt"), b"secret").expect("outside file");
        std::os::unix::fs::symlink(&outside, root.join("linked")).expect("symlink");
        let mut database = database(&directory.path().join("index.sqlite3"));
        let report = scan_library(
            &mut database,
            "library",
            &root,
            10,
            &ScanOptions::default(),
            |_| true,
        )
        .expect("scan");
        assert_eq!(report.progress.indexed_files, 0);
        assert_eq!(report.progress.skipped_entries, 1);
    }

    #[test]
    fn dirty_directories_coalesce_to_their_common_ancestor_event() {
        let directory = tempfile::tempdir().expect("temp directory");
        let root = directory.path().join("root");
        fs::create_dir_all(root.join("a/b")).expect("directories");
        let mut dirty = DirtyDirectorySet::new(&root).expect("dirty set");
        dirty.mark_event_path(&root.join("a/b/one.txt"));
        dirty.mark_event_path(&root.join("a/two.txt"));
        assert_eq!(
            dirty.drain(),
            Some(ReconcileRequest::Directories(vec![
                root.canonicalize().expect("canonical root").join("a")
            ]))
        );
    }

    #[test]
    fn full_snapshot_overrides_pending_incremental_work() {
        let directory = tempfile::tempdir().expect("temp directory");
        let root = directory.path().join("root");
        fs::create_dir(&root).expect("root");
        let mut dirty = DirtyDirectorySet::new(&root).expect("dirty set");
        dirty.mark_event_path(&root.join("changed.txt"));
        dirty.require_full_snapshot();
        assert_eq!(dirty.drain(), Some(ReconcileRequest::FullSnapshot));
        assert_eq!(dirty.drain(), None);
    }

    #[test]
    fn authoritative_snapshot_marks_deleted_files_missing() {
        let directory = tempfile::tempdir().expect("temp directory");
        let root = directory.path().join("root");
        fs::create_dir(&root).expect("root");
        let file = root.join("gone.txt");
        fs::write(&file, b"temporary").expect("file");
        let mut database = database(&directory.path().join("index.sqlite3"));
        reconcile_snapshot(
            &mut database,
            "library",
            &root,
            10,
            &ScanOptions::default(),
            |_| true,
        )
        .expect("initial snapshot");
        fs::remove_file(&file).expect("remove file");
        reconcile_snapshot(
            &mut database,
            "library",
            &root,
            20,
            &ScanOptions::default(),
            |_| true,
        )
        .expect("second snapshot");
        assert!(
            database
                .list_files_page("library", None, 10)
                .expect("visible files")
                .items
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_directory_marks_scan_incomplete_and_preserves_files() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("temp directory");
        let root = directory.path().join("root");
        let blocked = root.join("locked");
        fs::create_dir_all(&blocked).expect("create tree");
        fs::write(root.join("visible.txt"), b"visible").expect("visible file");
        fs::write(blocked.join("hidden.txt"), b"hidden").expect("hidden file");
        let mut database = database(&directory.path().join("scan.sqlite3"));
        reconcile_snapshot(
            &mut database,
            "library",
            &root,
            1000,
            &ScanOptions::default(),
            |_| true,
        )
        .expect("first scan");
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).expect("chmod blocked");
        let report = reconcile_snapshot(
            &mut database,
            "library",
            &root,
            2000,
            &ScanOptions::default(),
            |_| true,
        )
        .expect("second scan");
        assert!(report.incomplete);
        assert!(!report.cancelled);
        let _ = fs::set_permissions(&blocked, fs::Permissions::from_mode(0o755));
        let files = database
            .list_files_page("library", None, 10)
            .expect("list")
            .items;
        assert_eq!(files.len(), 2);
    }
}
