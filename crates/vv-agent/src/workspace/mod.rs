pub mod base;
pub mod local;
pub mod memory;
pub mod s3;

use std::collections::BTreeSet;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub use base::{FileInfo, WorkspaceBackend};
pub use local::LocalWorkspaceBackend;
pub use memory::MemoryWorkspaceBackend;
pub use s3::{S3WorkspaceBackend, S3WorkspaceConfig};

pub(crate) fn normalized_glob_pattern(glob: &str) -> String {
    let pattern = glob.trim();
    if pattern.is_empty() {
        "**/*".to_string()
    } else {
        pattern.replace('\\', "/")
    }
}

pub(super) fn path_to_posix(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub(super) fn absolutize_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

pub(crate) fn expand_home_path(raw_path: &str) -> PathBuf {
    if raw_path == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(raw_path));
    }
    if let Some(rest) = raw_path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    #[cfg(windows)]
    if let Some(rest) = raw_path.strip_prefix("~\\") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw_path)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .filter(|home| !home.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            if drive.is_empty() || path.is_empty() {
                return None;
            }
            Some(PathBuf::from(format!(
                "{}{}",
                drive.to_string_lossy(),
                path.to_string_lossy()
            )))
        })
}

pub(super) fn normalize_path_lexically(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

pub(super) fn normalize_workspace_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            value => parts.push(value),
        }
    }
    parts.join("/")
}

pub(super) fn suffix_with_dot(path: &str) -> String {
    let suffix = Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_string();
    if suffix.is_empty() {
        suffix
    } else {
        format!(".{suffix}")
    }
}

pub(super) fn system_time_to_utc_isoformat(time: SystemTime) -> String {
    let datetime: chrono::DateTime<chrono::Utc> = time.into();
    datetime.to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}

pub(super) fn current_utc_isoformat() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}

pub(super) fn insert_parent_dirs(dirs: &mut BTreeSet<String>, key: &str) {
    dirs.insert(String::new());
    let mut current = Vec::new();
    let mut parts = key.split('/').filter(|part| !part.is_empty()).peekable();
    while let Some(part) = parts.next() {
        current.push(part);
        if parts.peek().is_some() {
            dirs.insert(current.join("/"));
        }
    }
}

pub(super) fn not_found(path: &str) -> Error {
    Error::new(ErrorKind::NotFound, format!("path not found: {path}"))
}

pub(super) fn object_store_error_to_io(error: object_store::Error) -> Error {
    match error {
        object_store::Error::NotFound { path, source } => Error::new(
            ErrorKind::NotFound,
            format!("path not found: {path}: {source}"),
        ),
        other => Error::other(other.to_string()),
    }
}

pub(super) fn non_empty_option(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    })
}

pub(crate) fn glob_match(path: &str, pattern: &str) -> bool {
    glob_match_bytes(path.as_bytes(), pattern.as_bytes())
}

fn glob_match_bytes(path: &[u8], pattern: &[u8]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    if pattern.starts_with(b"**/") {
        return glob_match_bytes(path, &pattern[3..])
            || path
                .iter()
                .enumerate()
                .filter(|(_, value)| **value == b'/')
                .any(|(index, _)| glob_match_bytes(&path[index + 1..], &pattern[3..]));
    }
    if pattern.starts_with(b"**") {
        return (0..=path.len()).any(|index| glob_match_bytes(&path[index..], &pattern[2..]));
    }
    match pattern[0] {
        b'*' => {
            if glob_match_bytes(path, &pattern[1..]) {
                return true;
            }
            for index in 0..path.len() {
                if path[index] == b'/' {
                    break;
                }
                if glob_match_bytes(&path[index + 1..], &pattern[1..]) {
                    return true;
                }
            }
            false
        }
        b'?' => path
            .first()
            .is_some_and(|value| *value != b'/' && glob_match_bytes(&path[1..], &pattern[1..])),
        literal => path
            .first()
            .is_some_and(|value| *value == literal && glob_match_bytes(&path[1..], &pattern[1..])),
    }
}
