use std::any::Any;
use std::fs;
use std::io::{Error, ErrorKind, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{
    absolutize_path, artifacts::is_reserved_artifact_path, exclusive_workspace_path,
    expand_home_path, glob_match, normalize_path_lexically, normalize_workspace_path,
    normalized_glob_pattern, path_to_posix, suffix_with_dot, system_time_to_utc_isoformat,
    FileInfo, WorkspaceBackend,
};

const PRIVATE_ARTIFACT_ROOT_ENV: &str = "VV_AGENT_PRIVATE_ARTIFACT_ROOT";
const PRIVATE_ARTIFACT_ROOT_NAME: &str = "vv-agent-artifacts";

#[derive(Debug, Clone)]
pub struct LocalWorkspaceBackend {
    pub root: PathBuf,
    pub allow_outside_root: bool,
    artifact_root: PathBuf,
}

impl LocalWorkspaceBackend {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let normalized_root = root
            .canonicalize()
            .unwrap_or_else(|_| absolutize_path(&root));
        Self {
            root,
            allow_outside_root: false,
            artifact_root: private_artifact_root(&normalized_root),
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
        let normalized = resolve_existing_or_parent(&target)?;
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

    fn artifact_segments(&self, path: &str) -> std::io::Result<Option<Vec<String>>> {
        if Path::new(path).is_absolute() || path.starts_with('\\') {
            return Ok(None);
        }
        let normalized = normalize_workspace_path(path);
        if !is_reserved_artifact_path(&normalized) {
            return Ok(None);
        }
        let canonical = exclusive_workspace_path(path)?;
        Ok(Some(canonical.split('/').map(str::to_string).collect()))
    }

    fn resolve_artifact_path(&self, path: &str) -> std::io::Result<Option<(PathBuf, String)>> {
        let Some(segments) = self.artifact_segments(path)? else {
            return Ok(None);
        };
        ensure_existing_private_directory(&self.artifact_root)?;
        let mut target = self.artifact_root.clone();
        for segment in segments {
            target.push(segment);
            match fs::symlink_metadata(&target) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(Error::new(ErrorKind::InvalidInput, "artifact_path_invalid"));
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(Some((target, normalize_workspace_path(path))))
    }

    fn resolve_read_path(&self, path: &str) -> std::io::Result<(PathBuf, Option<String>)> {
        if let Some((target, logical_path)) = self.resolve_artifact_path(path)? {
            return Ok((target, Some(logical_path)));
        }
        Ok((self.resolve_path(path)?, None))
    }

    fn ensure_private_artifact_root(&self) -> std::io::Result<()> {
        let Some(base) = self.artifact_root.parent() else {
            return Err(Error::new(ErrorKind::InvalidInput, "artifact_path_invalid"));
        };
        ensure_private_directory(base)?;
        ensure_private_directory(&self.artifact_root)
    }

    fn is_reserved_target(&self, target: &Path) -> bool {
        target
            .strip_prefix(self.normalized_root())
            .ok()
            .map(path_to_posix)
            .is_some_and(|path| is_reserved_artifact_path(&path))
    }
}

fn resolve_existing_or_parent(path: &Path) -> std::io::Result<PathBuf> {
    if path.exists() {
        return path.canonicalize();
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let resolved_parent = if parent.exists() {
        parent.canonicalize()?
    } else {
        normalize_path_lexically(parent.to_path_buf())
    };
    Ok(match path.file_name() {
        Some(file_name) => resolved_parent.join(file_name),
        None => resolved_parent,
    })
}

fn private_artifact_root(workspace_root: &Path) -> PathBuf {
    let base = std::env::var_os(PRIVATE_ARTIFACT_ROOT_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(PRIVATE_ARTIFACT_ROOT_NAME));
    let digest = format!(
        "{:x}",
        Sha256::digest(workspace_root.to_string_lossy().as_bytes())
    );
    base.join(digest)
}

fn ensure_existing_private_directory(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(Error::new(ErrorKind::InvalidInput, "artifact_path_invalid"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn ensure_private_directory(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::new(ErrorKind::InvalidInput, "artifact_path_invalid"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

impl WorkspaceBackend for LocalWorkspaceBackend {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn list_files(&self, base: &str, glob: &str) -> std::io::Result<Vec<String>> {
        if self.artifact_segments(base)?.is_some() {
            return Ok(Vec::new());
        }
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
                    let path = self.output_path(&entry);
                    if !is_reserved_artifact_path(&path) {
                        files.push(path);
                    }
                }
            }
        }
        files.sort();
        Ok(files)
    }

    fn read_text(&self, path: &str) -> std::io::Result<String> {
        let (path, _) = self.resolve_read_path(path)?;
        let bytes = fs::read(path)?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    fn read_bytes(&self, path: &str) -> std::io::Result<Vec<u8>> {
        let (path, _) = self.resolve_read_path(path)?;
        fs::read(path)
    }

    fn write_text(&self, path: &str, content: &str, append: bool) -> std::io::Result<usize> {
        if is_reserved_artifact_path(path) {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "artifact paths are immutable",
            ));
        }
        let target = self.resolve_path(path)?;
        if self.is_reserved_target(&target) {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "artifact paths are immutable",
            ));
        }
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

    fn write_text_exclusive(&self, path: &str, content: &str) -> std::io::Result<usize> {
        let mut chunks = std::iter::once(Ok(content.to_string()));
        self.write_text_chunks_exclusive(path, &mut chunks)
    }

    fn write_text_chunks_exclusive(
        &self,
        path: &str,
        chunks: &mut dyn Iterator<Item = std::io::Result<String>>,
    ) -> std::io::Result<usize> {
        let root = if self.artifact_segments(path)?.is_some() {
            self.ensure_private_artifact_root()?;
            self.artifact_root.clone()
        } else {
            let root = self.normalized_root();
            fs::create_dir_all(&root)?;
            root
        };
        write_text_chunks_exclusive_below(&root, path, chunks)
    }

    fn file_info(&self, path: &str) -> std::io::Result<Option<FileInfo>> {
        let (target, logical_path) = self.resolve_read_path(path)?;
        if !target.exists() {
            return Ok(None);
        }
        let metadata = fs::metadata(&target)?;
        let modified_at = metadata
            .modified()
            .map(system_time_to_utc_isoformat)
            .unwrap_or_else(|_| system_time_to_utc_isoformat(std::time::SystemTime::UNIX_EPOCH));
        Ok(Some(FileInfo {
            path: logical_path.unwrap_or_else(|| self.output_path(&target)),
            is_file: metadata.is_file(),
            is_dir: metadata.is_dir(),
            size: metadata.len(),
            modified_at,
            suffix: suffix_with_dot(&target.to_string_lossy()),
        }))
    }

    fn exists(&self, path: &str) -> bool {
        self.resolve_read_path(path)
            .map(|(path, _)| path.exists())
            .unwrap_or(false)
    }

    fn is_file(&self, path: &str) -> bool {
        self.resolve_read_path(path)
            .map(|(path, _)| path.is_file())
            .unwrap_or(false)
    }

    fn mkdir(&self, path: &str) -> std::io::Result<()> {
        if is_reserved_artifact_path(path) {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "artifact paths are immutable",
            ));
        }
        let target = self.resolve_path(path)?;
        if self.is_reserved_target(&target) {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "artifact paths are immutable",
            ));
        }
        fs::create_dir_all(target)
    }
}

#[cfg(unix)]
fn write_text_chunks_exclusive_below(
    root: &Path,
    path: &str,
    chunks: &mut dyn Iterator<Item = std::io::Result<String>>,
) -> std::io::Result<usize> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};

    let canonical = exclusive_workspace_path(path)?;
    let segments = canonical.split('/').collect::<Vec<_>>();
    let mut directory = fs::File::open(root)?;
    for segment in &segments[..segments.len() - 1] {
        let segment = CString::new(*segment)
            .map_err(|_| Error::new(ErrorKind::InvalidInput, "path contains NUL"))?;
        let created = unsafe { libc::mkdirat(directory.as_raw_fd(), segment.as_ptr(), 0o700) };
        if created == -1 {
            let error = Error::last_os_error();
            if error.kind() != ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                segment.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd == -1 {
            let error = Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "exclusive path traverses a symlink or non-directory segment",
                ));
            }
            return Err(error);
        }
        directory = unsafe { fs::File::from_raw_fd(fd) };
    }

    let filename = CString::new(*segments.last().expect("non-empty segments"))
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "path contains NUL"))?;
    let mut temporary_name = None;
    let mut temporary_file = None;
    for _ in 0..32 {
        let candidate = CString::new(format!(
            ".vv-agent-write-{}.tmp",
            uuid::Uuid::new_v4().simple()
        ))
        .expect("UUID temporary filename contains no NUL");
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                candidate.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd != -1 {
            temporary_name = Some(candidate);
            temporary_file = Some(unsafe { fs::File::from_raw_fd(fd) });
            break;
        }
        let error = Error::last_os_error();
        if error.kind() != ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    let temporary_name = temporary_name.ok_or_else(|| {
        Error::new(
            ErrorKind::AlreadyExists,
            "could not allocate an exclusive temporary artifact path",
        )
    })?;
    let mut file = temporary_file.expect("temporary file accompanies its name");
    let write_result = write_chunks(&mut file, chunks).and_then(|written| {
        file.sync_all()?;
        Ok(written)
    });
    drop(file);
    let written = match write_result {
        Ok(written) => written,
        Err(error) => {
            unsafe {
                libc::unlinkat(directory.as_raw_fd(), temporary_name.as_ptr(), 0);
            }
            return Err(error);
        }
    };

    let published = unsafe {
        libc::linkat(
            directory.as_raw_fd(),
            temporary_name.as_ptr(),
            directory.as_raw_fd(),
            filename.as_ptr(),
            0,
        )
    };
    if published == -1 {
        let error = Error::last_os_error();
        unsafe {
            libc::unlinkat(directory.as_raw_fd(), temporary_name.as_ptr(), 0);
        }
        return Err(error);
    }
    let unlinked = unsafe { libc::unlinkat(directory.as_raw_fd(), temporary_name.as_ptr(), 0) };
    if unlinked == -1 {
        let error = Error::last_os_error();
        unsafe {
            libc::unlinkat(directory.as_raw_fd(), filename.as_ptr(), 0);
            libc::unlinkat(directory.as_raw_fd(), temporary_name.as_ptr(), 0);
        }
        return Err(error);
    }
    if let Err(error) = directory.sync_all() {
        unsafe {
            libc::unlinkat(directory.as_raw_fd(), filename.as_ptr(), 0);
        }
        return Err(error);
    }
    Ok(written)
}

#[cfg(not(unix))]
fn write_text_chunks_exclusive_below(
    root: &Path,
    path: &str,
    chunks: &mut dyn Iterator<Item = std::io::Result<String>>,
) -> std::io::Result<usize> {
    let canonical = exclusive_workspace_path(path)?;
    let segments = canonical.split('/').collect::<Vec<_>>();
    let mut parent = root.to_path_buf();
    for segment in &segments[..segments.len() - 1] {
        parent.push(segment);
        match fs::symlink_metadata(&parent) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "exclusive path contains a symlink",
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "exclusive path parent is not a directory",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => fs::create_dir(&parent)?,
            Err(error) => return Err(error),
        }
    }
    let target = parent.join(segments.last().expect("non-empty segments"));
    let mut temporary_path = None;
    let mut temporary_file = None;
    for _ in 0..32 {
        let candidate = parent.join(format!(
            ".vv-agent-write-{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        match fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&candidate)
        {
            Ok(file) => {
                temporary_path = Some(candidate);
                temporary_file = Some(file);
                break;
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let temporary_path = temporary_path.ok_or_else(|| {
        Error::new(
            ErrorKind::AlreadyExists,
            "could not allocate an exclusive temporary artifact path",
        )
    })?;
    let mut file = temporary_file.expect("temporary file accompanies its path");
    let write_result = write_chunks(&mut file, chunks).and_then(|written| {
        file.sync_all()?;
        Ok(written)
    });
    drop(file);
    let written = match write_result {
        Ok(written) => written,
        Err(error) => {
            let _ = fs::remove_file(&temporary_path);
            return Err(error);
        }
    };
    if let Err(error) = fs::hard_link(&temporary_path, &target) {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    if let Err(error) = fs::remove_file(&temporary_path) {
        let _ = fs::remove_file(&target);
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    if let Err(error) = fs::File::open(&parent).and_then(|directory| directory.sync_all()) {
        let _ = fs::remove_file(&target);
        return Err(error);
    }
    Ok(written)
}

fn write_chunks(
    file: &mut fs::File,
    chunks: &mut dyn Iterator<Item = std::io::Result<String>>,
) -> std::io::Result<usize> {
    let mut written = 0usize;
    for chunk in chunks {
        let chunk = chunk?;
        written = written
            .checked_add(chunk.len())
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "artifact output is too large"))?;
        file.write_all(chunk.as_bytes())?;
    }
    Ok(written)
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
