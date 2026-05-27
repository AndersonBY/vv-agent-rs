use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use super::{
    glob_match, insert_parent_dirs, normalize_workspace_path, normalized_glob_pattern, not_found,
    suffix_with_dot, FileInfo, WorkspaceBackend,
};

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
    fn as_any(&self) -> &dyn Any {
        self
    }

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
                suffix: suffix_with_dot(path),
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
