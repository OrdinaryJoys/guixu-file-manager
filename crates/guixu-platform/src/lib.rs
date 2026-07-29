use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;

use guixu_domain::{FileIdentity, FileSnapshot, Platform};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedFile {
    pub identity: FileIdentity,
    pub snapshot: FileSnapshot,
}

#[derive(Debug, Error)]
pub enum ObserveError {
    #[error("无法读取文件元数据：{0}")]
    Io(#[from] std::io::Error),
    #[error("不跟随符号链接")]
    Symlink,
    #[error("该路径不是普通文件")]
    NotAFile,
    #[error("当前平台的稳定文件身份适配器尚未实现")]
    UnsupportedPlatform,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileLaunchAction {
    Open,
    Reveal,
}

#[derive(Debug, Error)]
pub enum LaunchError {
    #[error("无法启动系统文件处理程序：{0}")]
    Io(#[from] std::io::Error),
    #[error("文件没有可显示的父目录")]
    MissingParent,
    #[error("当前平台的文件打开适配器尚未实现")]
    UnsupportedPlatform,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LaunchSpec {
    program: &'static str,
    args: Vec<OsString>,
}

pub fn launch_file(path: impl AsRef<Path>, action: FileLaunchAction) -> Result<(), LaunchError> {
    let spec = launch_spec(path.as_ref(), action)?;
    Command::new(spec.program).args(spec.args).spawn()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn launch_spec(path: &Path, action: FileLaunchAction) -> Result<LaunchSpec, LaunchError> {
    let mut args = Vec::with_capacity(2);
    if action == FileLaunchAction::Reveal {
        args.push(OsString::from("-R"));
    }
    args.push(path.as_os_str().to_owned());
    Ok(LaunchSpec {
        program: "open",
        args,
    })
}

#[cfg(target_os = "linux")]
fn launch_spec(path: &Path, action: FileLaunchAction) -> Result<LaunchSpec, LaunchError> {
    let target = match action {
        FileLaunchAction::Open => path,
        FileLaunchAction::Reveal => path.parent().ok_or(LaunchError::MissingParent)?,
    };
    Ok(LaunchSpec {
        program: "xdg-open",
        args: vec![target.as_os_str().to_owned()],
    })
}

#[cfg(target_os = "windows")]
fn launch_spec(path: &Path, action: FileLaunchAction) -> Result<LaunchSpec, LaunchError> {
    let args = match action {
        FileLaunchAction::Open => vec![path.as_os_str().to_owned()],
        FileLaunchAction::Reveal => vec![OsString::from("/select,"), path.as_os_str().to_owned()],
    };
    Ok(LaunchSpec {
        program: "explorer.exe",
        args,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn launch_spec(_path: &Path, _action: FileLaunchAction) -> Result<LaunchSpec, LaunchError> {
    Err(LaunchError::UnsupportedPlatform)
}

pub fn observe_file(path: impl AsRef<Path>) -> Result<ObservedFile, ObserveError> {
    let path_ref = path.as_ref();
    let metadata = fs::symlink_metadata(path_ref)?;
    if metadata.file_type().is_symlink() {
        return Err(ObserveError::Symlink);
    }
    if !metadata.is_file() {
        return Err(ObserveError::NotAFile);
    }
    observe_metadata(path_ref, &metadata)
}

#[cfg(target_os = "macos")]
fn observe_metadata(path: &Path, metadata: &fs::Metadata) -> Result<ObservedFile, ObserveError> {
    use std::os::macos::fs::MetadataExt;

    // P4：通过 libc::stat 取 st_gen 作为世代号，加固 inode 复用检测。
    let generation = {
        let path_c = std::ffi::CString::new(
            path.as_os_str()
                .to_str()
                .ok_or_else(|| ObserveError::Io(std::io::Error::other("non-UTF-8 path")))?,
        )
        .map_err(|_| ObserveError::Io(std::io::Error::other("path contains NUL")))?;
        let mut stat_buf: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::stat(path_c.as_ptr(), &mut stat_buf) } == 0 {
            Some(stat_buf.st_gen.to_string())
        } else {
            None
        }
    };
    Ok(ObservedFile {
        identity: FileIdentity {
            platform: Platform::MacOs,
            volume_id: metadata.st_dev().to_string(),
            native_file_id: metadata.st_ino().to_string(),
            generation,
        },
        snapshot: FileSnapshot {
            size: metadata.st_size(),
            modified_at_ns: seconds_and_nanos(metadata.st_mtime(), metadata.st_mtime_nsec()),
            changed_at_ns: seconds_and_nanos(metadata.st_ctime(), metadata.st_ctime_nsec()),
            created_at_ns: Some(seconds_and_nanos(
                metadata.st_birthtime(),
                metadata.st_birthtime_nsec(),
            )),
        },
    })
}

#[cfg(target_os = "linux")]
fn observe_metadata(_path: &Path, metadata: &fs::Metadata) -> Result<ObservedFile, ObserveError> {
    use std::os::linux::fs::MetadataExt;

    Ok(ObservedFile {
        identity: FileIdentity {
            platform: Platform::Linux,
            volume_id: metadata.st_dev().to_string(),
            native_file_id: metadata.st_ino().to_string(),
            generation: None,
        },
        snapshot: FileSnapshot {
            size: metadata.st_size(),
            modified_at_ns: seconds_and_nanos(metadata.st_mtime(), metadata.st_mtime_nsec()),
            changed_at_ns: seconds_and_nanos(metadata.st_ctime(), metadata.st_ctime_nsec()),
            created_at_ns: None,
        },
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn observe_metadata(_path: &Path, _metadata: &fs::Metadata) -> Result<ObservedFile, ObserveError> {
    Err(ObserveError::UnsupportedPlatform)
}

fn seconds_and_nanos(seconds: i64, nanos: i64) -> i64 {
    seconds.saturating_mul(1_000_000_000).saturating_add(nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_survives_a_rename() {
        let directory = tempfile::tempdir().expect("temp directory");
        let before_path = directory.path().join("before.txt");
        let after_path = directory.path().join("after.txt");
        fs::write(&before_path, b"identity").expect("write fixture");
        let before = observe_file(&before_path).expect("observe before");
        fs::rename(&before_path, &after_path).expect("rename fixture");
        let after = observe_file(&after_path).expect("observe after");
        assert_eq!(before.identity, after.identity);
        assert_eq!(before.snapshot.size, after.snapshot.size);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symbolic_links() {
        let directory = tempfile::tempdir().expect("temp directory");
        let source = directory.path().join("source.txt");
        let link = directory.path().join("link.txt");
        fs::write(&source, b"source").expect("write fixture");
        std::os::unix::fs::symlink(&source, &link).expect("make symlink");
        assert!(matches!(observe_file(link), Err(ObserveError::Symlink)));
    }

    #[test]
    fn launch_spec_passes_paths_as_arguments_without_a_shell() {
        let path = Path::new("/tmp/report; touch should-not-run.txt");
        let open = launch_spec(path, FileLaunchAction::Open).expect("open spec");
        assert!(open.args.contains(&path.as_os_str().to_owned()));

        let reveal = launch_spec(path, FileLaunchAction::Reveal).expect("reveal spec");
        #[cfg(target_os = "macos")]
        assert_eq!(reveal.args, vec![OsString::from("-R"), path.into()]);
        #[cfg(target_os = "linux")]
        assert_eq!(reveal.args, vec![OsString::from("/tmp")]);
    }
}
