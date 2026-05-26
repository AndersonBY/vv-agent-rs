use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    pub path: String,
    pub is_file: bool,
    pub is_dir: bool,
    pub size: u64,
    pub modified_at: String,
    pub suffix: String,
}

pub trait WorkspaceBackend: Send + Sync {
    fn list_files(&self, base: &str, glob: &str) -> std::io::Result<Vec<String>>;
    fn read_text(&self, path: &str) -> std::io::Result<String>;
    fn read_bytes(&self, path: &str) -> std::io::Result<Vec<u8>>;
    fn write_text(&self, path: &str, content: &str, append: bool) -> std::io::Result<usize>;
    fn file_info(&self, path: &str) -> std::io::Result<Option<FileInfo>>;
    fn exists(&self, path: &str) -> bool;
    fn is_file(&self, path: &str) -> bool;
    fn mkdir(&self, path: &str) -> std::io::Result<()>;
}

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

    fn resolve_path(&self, path: &str) -> PathBuf {
        let candidate = Path::new(path);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.root.join(candidate)
        }
    }
}

impl WorkspaceBackend for LocalWorkspaceBackend {
    fn list_files(&self, base: &str, _glob: &str) -> std::io::Result<Vec<String>> {
        let root = self.resolve_path(base);
        let mut files = Vec::new();
        if root.exists() {
            for entry in walk_recursive(&root)? {
                if entry.is_file() {
                    if let Ok(relative) = entry.strip_prefix(&self.root) {
                        files.push(relative.to_string_lossy().replace('\\', "/"));
                    }
                }
            }
        }
        Ok(files)
    }

    fn read_text(&self, path: &str) -> std::io::Result<String> {
        fs::read_to_string(self.resolve_path(path))
    }

    fn read_bytes(&self, path: &str) -> std::io::Result<Vec<u8>> {
        fs::read(self.resolve_path(path))
    }

    fn write_text(&self, path: &str, content: &str, append: bool) -> std::io::Result<usize> {
        let target = self.resolve_path(path);
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
        let target = self.resolve_path(path);
        if !target.exists() {
            return Ok(None);
        }
        let metadata = fs::metadata(&target)?;
        let modified_at = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs().to_string())
            .unwrap_or_else(|| "0".to_string());
        Ok(Some(FileInfo {
            path: path.to_string(),
            is_file: metadata.is_file(),
            is_dir: metadata.is_dir(),
            size: metadata.len(),
            modified_at,
            suffix: target
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default()
                .to_string(),
        }))
    }

    fn exists(&self, path: &str) -> bool {
        self.resolve_path(path).exists()
    }

    fn is_file(&self, path: &str) -> bool {
        self.resolve_path(path).is_file()
    }

    fn mkdir(&self, path: &str) -> std::io::Result<()> {
        fs::create_dir_all(self.resolve_path(path))
    }
}

#[derive(Debug, Clone, Default)]
pub struct MemoryWorkspaceBackend {
    files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
}

impl WorkspaceBackend for MemoryWorkspaceBackend {
    fn list_files(&self, base: &str, _glob: &str) -> std::io::Result<Vec<String>> {
        let prefix = base.trim_matches('/').to_string();
        let files = self.files.lock().expect("memory workspace poisoned");
        Ok(files
            .keys()
            .filter(|path| prefix.is_empty() || path.starts_with(&prefix))
            .cloned()
            .collect())
    }

    fn read_text(&self, path: &str) -> std::io::Result<String> {
        let files = self.files.lock().expect("memory workspace poisoned");
        Ok(String::from_utf8(files.get(path).cloned().unwrap_or_default()).unwrap_or_default())
    }

    fn read_bytes(&self, path: &str) -> std::io::Result<Vec<u8>> {
        let files = self.files.lock().expect("memory workspace poisoned");
        Ok(files.get(path).cloned().unwrap_or_default())
    }

    fn write_text(&self, path: &str, content: &str, append: bool) -> std::io::Result<usize> {
        let mut files = self.files.lock().expect("memory workspace poisoned");
        let entry = files.entry(path.to_string()).or_default();
        if append {
            entry.extend_from_slice(content.as_bytes());
        } else {
            *entry = content.as_bytes().to_vec();
        }
        Ok(content.len())
    }

    fn file_info(&self, path: &str) -> std::io::Result<Option<FileInfo>> {
        let files = self.files.lock().expect("memory workspace poisoned");
        Ok(files.get(path).map(|content| FileInfo {
            path: path.to_string(),
            is_file: true,
            is_dir: false,
            size: content.len() as u64,
            modified_at: "0".to_string(),
            suffix: Path::new(path)
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default()
                .to_string(),
        }))
    }

    fn exists(&self, path: &str) -> bool {
        self.files
            .lock()
            .expect("memory workspace poisoned")
            .contains_key(path)
    }

    fn is_file(&self, path: &str) -> bool {
        self.exists(path)
    }

    fn mkdir(&self, _path: &str) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct S3WorkspaceBackend;

impl WorkspaceBackend for S3WorkspaceBackend {
    fn list_files(&self, _base: &str, _glob: &str) -> std::io::Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn read_text(&self, _path: &str) -> std::io::Result<String> {
        Ok(String::new())
    }

    fn read_bytes(&self, _path: &str) -> std::io::Result<Vec<u8>> {
        Ok(Vec::new())
    }

    fn write_text(&self, _path: &str, _content: &str, _append: bool) -> std::io::Result<usize> {
        Ok(0)
    }

    fn file_info(&self, _path: &str) -> std::io::Result<Option<FileInfo>> {
        Ok(None)
    }

    fn exists(&self, _path: &str) -> bool {
        false
    }

    fn is_file(&self, _path: &str) -> bool {
        false
    }

    fn mkdir(&self, _path: &str) -> std::io::Result<()> {
        Ok(())
    }
}

fn walk_recursive(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut stack = vec![root.to_path_buf()];
    let mut entries = Vec::new();
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path)? {
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
