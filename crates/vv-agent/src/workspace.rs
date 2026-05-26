use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::future::Future;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use futures_util::TryStreamExt;
use object_store::aws::AmazonS3Builder;
use object_store::path::Path as ObjectPath;
use object_store::{memory::InMemory, ObjectStore, ObjectStoreExt, PutPayload};
use serde::{Deserialize, Serialize};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    pub path: String,
    pub is_file: bool,
    pub is_dir: bool,
    pub size: u64,
    pub modified_at: String,
    pub suffix: String,
}

pub trait WorkspaceBackend: Any + Send + Sync {
    fn as_any(&self) -> &dyn Any;
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

    fn resolve_path(&self, path: &str) -> std::io::Result<PathBuf> {
        let root = self.normalized_root();
        let candidate = Path::new(path);
        let target = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            root.join(candidate)
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
        fs::read_to_string(self.resolve_path(path)?)
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
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs().to_string())
            .unwrap_or_else(|| "0".to_string());
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3WorkspaceConfig {
    pub bucket: String,
    pub prefix: String,
    pub endpoint_url: Option<String>,
    pub region_name: Option<String>,
    pub aws_access_key_id: Option<String>,
    pub aws_secret_access_key: Option<String>,
    pub aws_session_token: Option<String>,
    pub addressing_style: String,
}

impl S3WorkspaceConfig {
    pub fn new(bucket: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            prefix: String::new(),
            endpoint_url: None,
            region_name: None,
            aws_access_key_id: None,
            aws_secret_access_key: None,
            aws_session_token: None,
            addressing_style: "virtual".to_string(),
        }
    }
}

#[derive(Clone)]
pub struct S3WorkspaceBackend {
    store: Arc<dyn ObjectStore>,
    prefix: String,
    runtime: Arc<Runtime>,
}

impl std::fmt::Debug for S3WorkspaceBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("S3WorkspaceBackend")
            .field("store", &self.store.to_string())
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl Default for S3WorkspaceBackend {
    fn default() -> Self {
        Self::from_object_store(InMemory::new(), "")
            .expect("in-memory S3 workspace backend must initialize")
    }
}

impl S3WorkspaceBackend {
    pub fn new(bucket: impl Into<String>) -> std::io::Result<Self> {
        Self::from_config(S3WorkspaceConfig::new(bucket))
    }

    pub fn from_config(config: S3WorkspaceConfig) -> std::io::Result<Self> {
        let bucket = config.bucket.trim();
        if bucket.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "S3 bucket cannot be empty",
            ));
        }
        let mut builder = AmazonS3Builder::from_env().with_bucket_name(bucket.to_string());
        if let Some(endpoint) = non_empty_option(config.endpoint_url) {
            if endpoint.starts_with("http://") {
                builder = builder.with_allow_http(true);
            }
            builder = builder.with_endpoint(endpoint);
        }
        if let Some(region) = non_empty_option(config.region_name) {
            builder = builder.with_region(region);
        }
        if let Some(access_key_id) = non_empty_option(config.aws_access_key_id) {
            builder = builder.with_access_key_id(access_key_id);
        }
        if let Some(secret_access_key) = non_empty_option(config.aws_secret_access_key) {
            builder = builder.with_secret_access_key(secret_access_key);
        }
        if let Some(token) = non_empty_option(config.aws_session_token) {
            builder = builder.with_token(token);
        }
        let virtual_hosted = match config.addressing_style.trim().to_ascii_lowercase().as_str() {
            "" | "virtual" => true,
            "path" => false,
            other => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("unsupported S3 addressing_style: {other}"),
                ))
            }
        };
        builder = builder.with_virtual_hosted_style_request(virtual_hosted);
        let store = builder.build().map_err(object_store_error_to_io)?;
        Self::from_object_store(store, config.prefix)
    }

    pub fn from_object_store(
        store: impl ObjectStore + 'static,
        prefix: impl Into<String>,
    ) -> std::io::Result<Self> {
        Ok(Self {
            store: Arc::new(store),
            prefix: normalize_workspace_path(&prefix.into()),
            runtime: Arc::new(
                RuntimeBuilder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| Error::other(error.to_string()))?,
            ),
        })
    }

    fn object_key(&self, path: &str) -> String {
        let path = normalize_workspace_path(path);
        match (self.prefix.is_empty(), path.is_empty()) {
            (true, _) => path,
            (false, true) => self.prefix.clone(),
            (false, false) => format!("{}/{}", self.prefix, path),
        }
    }

    fn relative_key(&self, key: &str) -> Option<String> {
        if self.prefix.is_empty() {
            return Some(normalize_workspace_path(key));
        }
        if key == self.prefix {
            return Some(String::new());
        }
        key.strip_prefix(&format!("{}/", self.prefix))
            .map(normalize_workspace_path)
    }

    fn list_prefix(&self) -> Option<ObjectPath> {
        if self.prefix.is_empty() {
            None
        } else {
            Some(ObjectPath::from(self.prefix.clone()))
        }
    }

    fn block_on<T>(
        &self,
        future: impl Future<Output = object_store::Result<T>>,
    ) -> std::io::Result<T> {
        self.runtime
            .block_on(future)
            .map_err(object_store_error_to_io)
    }
}

impl WorkspaceBackend for S3WorkspaceBackend {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn list_files(&self, base: &str, glob: &str) -> std::io::Result<Vec<String>> {
        let base = normalize_workspace_path(base);
        let pattern = if base.is_empty() {
            normalized_glob_pattern(glob)
        } else {
            format!("{base}/{}", normalized_glob_pattern(glob))
        };
        let prefix = self.list_prefix();
        let objects = self.block_on(async {
            self.store
                .list(prefix.as_ref())
                .try_collect::<Vec<_>>()
                .await
        })?;
        let mut files = objects
            .into_iter()
            .filter_map(|object| self.relative_key(object.location.as_ref()))
            .filter(|path| !path.is_empty() && !path.ends_with('/'))
            .filter(|path| glob_match(path, &pattern))
            .collect::<Vec<_>>();
        files.sort();
        Ok(files)
    }

    fn read_text(&self, path: &str) -> std::io::Result<String> {
        let bytes = self.read_bytes(path)?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    fn read_bytes(&self, path: &str) -> std::io::Result<Vec<u8>> {
        let key = ObjectPath::from(self.object_key(path));
        self.block_on(async { Ok(self.store.get(&key).await?.bytes().await?.to_vec()) })
    }

    fn write_text(&self, path: &str, content: &str, append: bool) -> std::io::Result<usize> {
        let key = ObjectPath::from(self.object_key(path));
        let content = if append {
            match self.read_text(path) {
                Ok(existing) => existing + content,
                Err(error) if error.kind() == ErrorKind::NotFound => content.to_string(),
                Err(error) => return Err(error),
            }
        } else {
            content.to_string()
        };
        let len = content.len();
        self.block_on(async {
            self.store
                .put(&key, PutPayload::from(content.into_bytes()))
                .await
                .map(|_| ())
        })?;
        Ok(len)
    }

    fn file_info(&self, path: &str) -> std::io::Result<Option<FileInfo>> {
        let key = ObjectPath::from(self.object_key(path));
        let metadata = match self.block_on(async { self.store.head(&key).await }) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let object_path = metadata.location.as_ref();
        Ok(Some(FileInfo {
            path: self
                .relative_key(object_path)
                .unwrap_or_else(|| normalize_workspace_path(path)),
            is_file: true,
            is_dir: false,
            size: metadata.size,
            modified_at: metadata.last_modified.to_rfc3339(),
            suffix: suffix_with_dot(path),
        }))
    }

    fn exists(&self, path: &str) -> bool {
        self.file_info(path).ok().flatten().is_some()
    }

    fn is_file(&self, path: &str) -> bool {
        self.exists(path)
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

fn absolutize_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn normalize_path_lexically(path: PathBuf) -> PathBuf {
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

fn suffix_with_dot(path: &str) -> String {
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

fn object_store_error_to_io(error: object_store::Error) -> Error {
    match error {
        object_store::Error::NotFound { path, source } => Error::new(
            ErrorKind::NotFound,
            format!("path not found: {path}: {source}"),
        ),
        other => Error::other(other.to_string()),
    }
}

fn non_empty_option(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    })
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
