use std::any::Any;
use std::fs;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

use super::{
    absolutize_path, expand_home_path, glob_match, normalize_path_lexically,
    normalized_glob_pattern, path_to_posix, suffix_with_dot, system_time_to_utc_isoformat,
    FileInfo, WorkspaceBackend,
};

#[derive(Debug, Clone)]
pub struct LocalWorkspaceBackend {
    pub root: PathBuf,
    pub allow_outside_root: bool,
}

impl LocalWorkspaceBackend {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            allow_outside_root: false,
        }
    }

    fn resolve_path(&self, path: &str) -> std::io::Result<PathBuf> {
        let root = self.normalized_root();
        let candidate = expand_home_path(path);
        let target = if candidate.is_absolute() {
            candidate
        } else {
            root.join(&candidate)
        };
        let normalized = normalize_path_lexically(target);
        if !self.allow_outside_root && normalized != root && !normalized.starts_with(&root) {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                format!("Path escapes workspace: {path}"),
            ));
        }
        Ok(normalized)
    }

    fn normalized_root(&self) -> PathBuf {
        self.root
            .canonicalize()
            .unwrap_or_else(|_| absolutize_path(&self.root))
    }

    fn output_path(&self, path: &Path) -> String {
        let root = self.normalized_root();
        if let Ok(relative) = path.strip_prefix(&root) {
            let output = path_to_posix(relative);
            if output.is_empty() {
                ".".to_string()
            } else {
                output
            }
        } else {
            path.to_string_lossy().to_string()
        }
    }
}

impl WorkspaceBackend for LocalWorkspaceBackend {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn list_files(&self, base: &str, glob: &str) -> std::io::Result<Vec<String>> {
        let root = self.resolve_path(base)?;
        let mut files = Vec::new();
        if root.exists() && root.is_dir() {
            let pattern = normalized_glob_pattern(glob);
            for entry in walk_recursive(&root)? {
                if entry.is_file() {
                    let Ok(relative_from_base) = entry.strip_prefix(&root) else {
                        continue;
                    };
                    if !glob_match(&path_to_posix(relative_from_base), &pattern) {
                        continue;
                    }
                    files.push(self.output_path(&entry));
                }
            }
        }
        files.sort();
        Ok(files)
    }

    fn read_text(&self, path: &str) -> std::io::Result<String> {
        let bytes = fs::read(self.resolve_path(path)?)?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    fn read_bytes(&self, path: &str) -> std::io::Result<Vec<u8>> {
        fs::read(self.resolve_path(path)?)
    }

    fn write_text(&self, path: &str, content: &str, append: bool) -> std::io::Result<usize> {
        let target = self.resolve_path(path)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        if append {
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(target)?;
            file.write_all(content.as_bytes())?;
            Ok(content.len())
        } else {
            fs::write(&target, content)?;
            Ok(content.len())
        }
    }

    fn file_info(&self, path: &str) -> std::io::Result<Option<FileInfo>> {
        let target = self.resolve_path(path)?;
        if !target.exists() {
            return Ok(None);
        }
        let metadata = fs::metadata(&target)?;
        let modified_at = metadata
            .modified()
            .map(system_time_to_utc_isoformat)
            .unwrap_or_else(|_| system_time_to_utc_isoformat(std::time::SystemTime::UNIX_EPOCH));
        Ok(Some(FileInfo {
            path: self.output_path(&target),
            is_file: metadata.is_file(),
            is_dir: metadata.is_dir(),
            size: metadata.len(),
            modified_at,
            suffix: suffix_with_dot(&target.to_string_lossy()),
        }))
    }

    fn exists(&self, path: &str) -> bool {
        self.resolve_path(path)
            .map(|path| path.exists())
            .unwrap_or(false)
    }

    fn is_file(&self, path: &str) -> bool {
        self.resolve_path(path)
            .map(|path| path.is_file())
            .unwrap_or(false)
    }

    fn mkdir(&self, path: &str) -> std::io::Result<()> {
        fs::create_dir_all(self.resolve_path(path)?)
    }
}

fn walk_recursive(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut stack = vec![root.to_path_buf()];
    let mut entries = Vec::new();
    while let Some(path) = stack.pop() {
        let reader = match fs::read_dir(&path) {
            Ok(reader) => reader,
            Err(error) if path != root => {
                if error.kind() == ErrorKind::PermissionDenied {
                    continue;
                }
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        for entry in reader {
            let entry = entry?;
            let entry_path = entry.path();
            if entry_path.is_dir() {
                stack.push(entry_path.clone());
            }
            entries.push(entry_path);
        }
    }
    Ok(entries)
}
