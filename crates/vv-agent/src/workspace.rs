use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Error, ErrorKind};
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
    fn list_files(&self, base: &str, glob: &str) -> std::io::Result<Vec<String>> {
        let root = self.resolve_path(base);
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
                    if let Ok(relative) = entry.strip_prefix(&self.root) {
                        files.push(path_to_posix(relative));
                    }
                }
            }
        }
        files.sort();
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

#[derive(Debug, Clone)]
pub struct MemoryWorkspaceBackend {
    files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    dirs: Arc<Mutex<BTreeSet<String>>>,
}

impl Default for MemoryWorkspaceBackend {
    fn default() -> Self {
        let mut dirs = BTreeSet::new();
        dirs.insert(String::new());
        Self {
            files: Arc::new(Mutex::new(BTreeMap::new())),
            dirs: Arc::new(Mutex::new(dirs)),
        }
    }
}

impl WorkspaceBackend for MemoryWorkspaceBackend {
    fn list_files(&self, base: &str, glob: &str) -> std::io::Result<Vec<String>> {
        let base = normalize_workspace_path(base);
        let glob = normalized_glob_pattern(glob);
        let pattern = if base.is_empty() {
            glob
        } else {
            format!("{base}/{glob}")
        };
        let files = self.files.lock().expect("memory workspace poisoned");
        let mut matches = files
            .keys()
            .filter(|path| glob_match(path, &pattern))
            .cloned()
            .collect::<Vec<_>>();
        matches.sort();
        Ok(matches)
    }

    fn read_text(&self, path: &str) -> std::io::Result<String> {
        let bytes = self.read_bytes(path)?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    fn read_bytes(&self, path: &str) -> std::io::Result<Vec<u8>> {
        let key = normalize_workspace_path(path);
        let files = self.files.lock().expect("memory workspace poisoned");
        files.get(&key).cloned().ok_or_else(|| not_found(path))
    }

    fn write_text(&self, path: &str, content: &str, append: bool) -> std::io::Result<usize> {
        let key = normalize_workspace_path(path);
        let mut files = self.files.lock().expect("memory workspace poisoned");
        let entry = files.entry(key.clone()).or_default();
        if append {
            entry.extend_from_slice(content.as_bytes());
        } else {
            *entry = content.as_bytes().to_vec();
        }
        drop(files);
        self.ensure_parent_dirs(&key);
        Ok(content.len())
    }

    fn file_info(&self, path: &str) -> std::io::Result<Option<FileInfo>> {
        let key = normalize_workspace_path(path);
        let files = self.files.lock().expect("memory workspace poisoned");
        if let Some(content) = files.get(&key) {
            return Ok(Some(FileInfo {
                path: key,
                is_file: true,
                is_dir: false,
                size: content.len() as u64,
                modified_at: "0".to_string(),
                suffix: suffix_without_dot(path),
            }));
        }
        drop(files);
        let dirs = self.dirs.lock().expect("memory workspace poisoned");
        Ok(dirs.get(&key).map(|path| FileInfo {
            path: if path.is_empty() {
                ".".to_string()
            } else {
                path.clone()
            },
            is_file: false,
            is_dir: true,
            size: 0,
            modified_at: "0".to_string(),
            suffix: String::new(),
        }))
    }

    fn exists(&self, path: &str) -> bool {
        let key = normalize_workspace_path(path);
        self.files
            .lock()
            .expect("memory workspace poisoned")
            .contains_key(&key)
            || self
                .dirs
                .lock()
                .expect("memory workspace poisoned")
                .contains(&key)
    }

    fn is_file(&self, path: &str) -> bool {
        let key = normalize_workspace_path(path);
        self.files
            .lock()
            .expect("memory workspace poisoned")
            .contains_key(&key)
    }

    fn mkdir(&self, path: &str) -> std::io::Result<()> {
        let key = normalize_workspace_path(path);
        let mut dirs = self.dirs.lock().expect("memory workspace poisoned");
        dirs.insert(key.clone());
        insert_parent_dirs(&mut dirs, &key);
        Ok(())
    }
}

impl MemoryWorkspaceBackend {
    fn ensure_parent_dirs(&self, key: &str) {
        let mut dirs = self.dirs.lock().expect("memory workspace poisoned");
        insert_parent_dirs(&mut dirs, key);
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

fn normalized_glob_pattern(glob: &str) -> String {
    let pattern = glob.trim();
    if pattern.is_empty() {
        "**/*".to_string()
    } else {
        pattern.replace('\\', "/")
    }
}

fn path_to_posix(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn normalize_workspace_path(path: &str) -> String {
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

fn suffix_without_dot(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_string()
}

fn insert_parent_dirs(dirs: &mut BTreeSet<String>, key: &str) {
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

fn not_found(path: &str) -> Error {
    Error::new(ErrorKind::NotFound, format!("path not found: {path}"))
}

fn glob_match(path: &str, pattern: &str) -> bool {
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
