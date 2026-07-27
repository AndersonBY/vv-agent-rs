use std::fs::File;
use std::io::{Error, ErrorKind, Read};
use std::path::Path;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::types::ToolArtifactRef;

use super::WorkspaceBackend;

pub(crate) const BOUNDED_TEXT_CHARS: usize = 12_000;
const PREVIEW_HEAD_CHARS: usize = 6_000;
const PREVIEW_TAIL_CHARS: usize = 5_953;
const PREVIEW_MARKER: &str = "\n... output omitted; full text in artifact ...\n";
const CAPTURE_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundedTextPreview {
    pub content: String,
    pub original_bytes: u64,
    pub visible_bytes: u64,
    pub truncated: bool,
}

pub(crate) fn bounded_text_preview(text: &str) -> BoundedTextPreview {
    let original_bytes = text.len() as u64;
    if text.chars().count() <= BOUNDED_TEXT_CHARS {
        return BoundedTextPreview {
            content: text.to_string(),
            original_bytes,
            visible_bytes: original_bytes,
            truncated: false,
        };
    }
    let head = text.chars().take(PREVIEW_HEAD_CHARS).collect::<String>();
    let tail = text
        .chars()
        .rev()
        .take(PREVIEW_TAIL_CHARS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    let content = format!("{head}{PREVIEW_MARKER}{tail}");
    debug_assert_eq!(content.chars().count(), BOUNDED_TEXT_CHARS);
    BoundedTextPreview {
        visible_bytes: content.len() as u64,
        content,
        original_bytes,
        truncated: true,
    }
}

pub(crate) fn bounded_captured_text_preview(path: &Path) -> std::io::Result<BoundedTextPreview> {
    let mut first = String::new();
    let mut first_chars = 0usize;
    let mut tail = String::new();
    let mut total_chars = 0usize;
    let mut original_bytes = 0u64;

    for chunk in CapturedTextChunks::open(path)? {
        let chunk = chunk?;
        total_chars = total_chars.saturating_add(chunk.chars().count());
        original_bytes = original_bytes.saturating_add(chunk.len() as u64);
        if first_chars < BOUNDED_TEXT_CHARS {
            let prefix = prefix_chars(&chunk, BOUNDED_TEXT_CHARS - first_chars);
            first_chars += prefix.chars().count();
            first.push_str(prefix);
        }
        tail.push_str(&chunk);
        retain_tail_chars(&mut tail, PREVIEW_TAIL_CHARS);
    }

    if total_chars <= BOUNDED_TEXT_CHARS {
        return Ok(BoundedTextPreview {
            visible_bytes: original_bytes,
            content: first,
            original_bytes,
            truncated: false,
        });
    }

    let head = prefix_chars(&first, PREVIEW_HEAD_CHARS);
    let content = format!("{head}{PREVIEW_MARKER}{tail}");
    debug_assert_eq!(content.chars().count(), BOUNDED_TEXT_CHARS);
    Ok(BoundedTextPreview {
        visible_bytes: content.len() as u64,
        content,
        original_bytes,
        truncated: true,
    })
}

pub(crate) fn read_captured_text_prefix(path: &Path, limit_chars: usize) -> String {
    if limit_chars == 0 {
        return String::new();
    }
    let Ok(chunks) = CapturedTextChunks::open(path) else {
        return String::new();
    };
    let mut output = String::new();
    let mut visible_chars = 0usize;
    for chunk in chunks {
        let Ok(chunk) = chunk else {
            return String::new();
        };
        let prefix = prefix_chars(&chunk, limit_chars.saturating_sub(visible_chars));
        visible_chars += prefix.chars().count();
        output.push_str(prefix);
        if visible_chars == limit_chars {
            break;
        }
    }
    output
}

pub(crate) fn persist_captured_text_artifact(
    backend: Arc<dyn WorkspaceBackend>,
    task_id: &str,
    tool_call_id: &str,
    capture_path: &Path,
) -> std::io::Result<ToolArtifactRef> {
    let task = artifact_segment(task_id, "task");
    let call = artifact_segment(tool_call_id, "call");
    let mut last_collision = None;
    for _ in 0..32 {
        let suffix = Uuid::new_v4().simple().to_string();
        let path = format!(".vv-agent/artifacts/{task}/{call}-{suffix}.txt");
        let mut chunks = HashingCapturedTextChunks::open(capture_path)?;
        match backend.write_text_chunks_exclusive(&path, &mut chunks) {
            Ok(written) => {
                let (size_bytes, sha256) = chunks.finish();
                if written != size_bytes as usize {
                    return Err(Error::new(
                        ErrorKind::WriteZero,
                        format!("artifact write reported {written} of {size_bytes} bytes"),
                    ));
                }
                return Ok(ToolArtifactRef {
                    path,
                    media_type: "text/plain".to_string(),
                    encoding: "utf-8".to_string(),
                    size_bytes,
                    sha256,
                });
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                last_collision = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_collision.unwrap_or_else(|| {
        Error::new(
            ErrorKind::AlreadyExists,
            "could not allocate an exclusive artifact path",
        )
    }))
}

pub(crate) fn persist_text_artifact(
    backend: Arc<dyn WorkspaceBackend>,
    task_id: &str,
    tool_call_id: &str,
    content: &str,
) -> std::io::Result<ToolArtifactRef> {
    let task = artifact_segment(task_id, "task");
    let call = artifact_segment(tool_call_id, "call");
    let size_bytes = content.len() as u64;
    let sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
    let mut last_collision = None;
    for _ in 0..32 {
        let suffix = Uuid::new_v4().simple().to_string();
        let path = format!(".vv-agent/artifacts/{task}/{call}-{suffix}.txt");
        match backend.write_text_exclusive(&path, content) {
            Ok(written) => {
                if written != content.len() {
                    return Err(Error::new(
                        ErrorKind::WriteZero,
                        format!(
                            "artifact write reported {written} of {} bytes",
                            content.len()
                        ),
                    ));
                }
                return Ok(ToolArtifactRef {
                    path,
                    media_type: "text/plain".to_string(),
                    encoding: "utf-8".to_string(),
                    size_bytes,
                    sha256,
                });
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                last_collision = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_collision.unwrap_or_else(|| {
        Error::new(
            ErrorKind::AlreadyExists,
            "could not allocate an exclusive artifact path",
        )
    }))
}

pub(crate) fn read_validated_text_artifact(
    backend: &dyn WorkspaceBackend,
    artifact: &ToolArtifactRef,
) -> std::io::Result<String> {
    artifact
        .validate()
        .map_err(|error| Error::new(ErrorKind::InvalidInput, error))?;
    let bytes = backend.read_bytes(&artifact.path)?;
    if bytes.len() as u64 != artifact.size_bytes {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "artifact size_bytes does not match stored bytes",
        ));
    }
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    if sha256 != artifact.sha256 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "artifact sha256 does not match stored bytes",
        ));
    }
    String::from_utf8(bytes).map_err(|error| Error::new(ErrorKind::InvalidData, error))
}

pub(crate) fn artifact_write_error_code(error: &std::io::Error) -> &'static str {
    if error.kind() == ErrorKind::InvalidInput {
        "artifact_path_invalid"
    } else {
        "artifact_persist_failed"
    }
}

pub(super) fn is_reserved_artifact_path(path: &str) -> bool {
    super::normalize_workspace_path(path).starts_with(".vv-agent/artifacts/")
}

fn artifact_segment(value: &str, fallback: &str) -> String {
    let mut segment = String::with_capacity(64);
    for character in value.chars() {
        if segment.len() >= 64 {
            break;
        }
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            segment.push(character);
        } else if !segment.ends_with('-') {
            segment.push('-');
        }
    }
    while segment
        .chars()
        .next()
        .is_some_and(|character| !character.is_ascii_alphanumeric())
    {
        segment.remove(0);
    }
    if segment.is_empty() {
        fallback.to_string()
    } else {
        segment
    }
}

fn prefix_chars(value: &str, count: usize) -> &str {
    if count == 0 {
        return "";
    }
    value
        .char_indices()
        .nth(count)
        .map(|(index, _)| &value[..index])
        .unwrap_or(value)
}

fn retain_tail_chars(value: &mut String, count: usize) {
    let total = value.chars().count();
    if total <= count {
        return;
    }
    let remove_chars = total - count;
    let byte_index = value
        .char_indices()
        .nth(remove_chars)
        .map(|(index, _)| index)
        .unwrap_or(value.len());
    value.drain(..byte_index);
}

struct CapturedTextChunks {
    file: File,
    pending: Vec<u8>,
    eof: bool,
}

impl CapturedTextChunks {
    fn open(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            file: File::open(path)?,
            pending: Vec::with_capacity(CAPTURE_CHUNK_BYTES * 2),
            eof: false,
        })
    }

    fn next_decoded_chunk(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        match std::str::from_utf8(&self.pending) {
            Ok(text) => {
                let output = text.to_string();
                self.pending.clear();
                Some(output)
            }
            Err(error) if error.valid_up_to() > 0 => {
                let valid = error.valid_up_to();
                let output = String::from_utf8(self.pending.drain(..valid).collect())
                    .expect("valid UTF-8 prefix");
                Some(output)
            }
            Err(error) if error.error_len().is_some() => {
                let invalid_len = error.error_len().expect("checked invalid length");
                self.pending.drain(..invalid_len);
                Some("\u{fffd}".to_string())
            }
            Err(_) => None,
        }
    }
}

impl Iterator for CapturedTextChunks {
    type Item = std::io::Result<String>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(chunk) = self.next_decoded_chunk() {
                return Some(Ok(chunk));
            }
            if self.eof {
                if self.pending.is_empty() {
                    return None;
                }
                let output = String::from_utf8_lossy(&self.pending).into_owned();
                self.pending.clear();
                return Some(Ok(output));
            }

            let mut buffer = [0u8; CAPTURE_CHUNK_BYTES];
            match self.file.read(&mut buffer) {
                Ok(0) => self.eof = true,
                Ok(read) => self.pending.extend_from_slice(&buffer[..read]),
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

struct HashingCapturedTextChunks {
    inner: CapturedTextChunks,
    digest: Sha256,
    size_bytes: u64,
}

impl HashingCapturedTextChunks {
    fn open(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            inner: CapturedTextChunks::open(path)?,
            digest: Sha256::new(),
            size_bytes: 0,
        })
    }

    fn finish(self) -> (u64, String) {
        (self.size_bytes, format!("{:x}", self.digest.finalize()))
    }
}

impl Iterator for HashingCapturedTextChunks {
    type Item = std::io::Result<String>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|chunk| {
            chunk.inspect(|chunk| {
                self.size_bytes = self.size_bytes.saturating_add(chunk.len() as u64);
                self.digest.update(chunk.as_bytes());
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::io::ErrorKind;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::workspace::{FileInfo, MemoryWorkspaceBackend};

    struct ExclusiveProbeBackend {
        inner: MemoryWorkspaceBackend,
        collisions_remaining: AtomicUsize,
        failure: Option<ErrorKind>,
        attempted_paths: Mutex<Vec<String>>,
    }

    impl ExclusiveProbeBackend {
        fn new(collisions: usize, failure: Option<ErrorKind>) -> Self {
            Self {
                inner: MemoryWorkspaceBackend::default(),
                collisions_remaining: AtomicUsize::new(collisions),
                failure,
                attempted_paths: Mutex::new(Vec::new()),
            }
        }
    }

    impl WorkspaceBackend for ExclusiveProbeBackend {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn list_files(&self, base: &str, glob: &str) -> std::io::Result<Vec<String>> {
            self.inner.list_files(base, glob)
        }

        fn read_text(&self, path: &str) -> std::io::Result<String> {
            self.inner.read_text(path)
        }

        fn read_bytes(&self, path: &str) -> std::io::Result<Vec<u8>> {
            self.inner.read_bytes(path)
        }

        fn write_text(&self, path: &str, content: &str, append: bool) -> std::io::Result<usize> {
            self.inner.write_text(path, content, append)
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
            self.attempted_paths
                .lock()
                .expect("attempted paths")
                .push(path.to_string());
            if let Some(kind) = self.failure {
                return Err(Error::new(kind, "fixture artifact failure"));
            }
            if self
                .collisions_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(Error::new(ErrorKind::AlreadyExists, "fixture collision"));
            }
            self.inner.write_text_chunks_exclusive(path, chunks)
        }

        fn file_info(&self, path: &str) -> std::io::Result<Option<FileInfo>> {
            self.inner.file_info(path)
        }

        fn exists(&self, path: &str) -> bool {
            self.inner.exists(path)
        }

        fn is_file(&self, path: &str) -> bool {
            self.inner.is_file(path)
        }

        fn mkdir(&self, path: &str) -> std::io::Result<()> {
            self.inner.mkdir(path)
        }
    }

    fn capture_output(content: &str) -> tempfile::NamedTempFile {
        use std::io::Write;

        let mut capture = tempfile::NamedTempFile::new().expect("capture file");
        capture
            .write_all(content.as_bytes())
            .expect("capture output");
        capture
    }

    #[test]
    fn artifact_collision_selects_a_new_exclusive_path() {
        let backend = Arc::new(ExclusiveProbeBackend::new(1, None));
        let capture = capture_output("complete");
        let artifact =
            persist_captured_text_artifact(backend.clone(), "task-7", "call-bash", capture.path())
                .expect("artifact after collision");
        let attempted = backend.attempted_paths.lock().expect("attempted paths");
        assert_eq!(attempted.len(), 2);
        assert_ne!(attempted[0], attempted[1]);
        assert_eq!(artifact.path, attempted[1]);
        assert_eq!(backend.read_text(&artifact.path).unwrap(), "complete");
    }

    #[test]
    fn artifact_write_failure_is_not_reported_as_success() {
        let backend = Arc::new(ExclusiveProbeBackend::new(
            0,
            Some(ErrorKind::PermissionDenied),
        ));
        let capture = capture_output("complete");
        let error = persist_captured_text_artifact(backend, "task-7", "call-bash", capture.path())
            .expect_err("artifact persistence must fail");
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(artifact_write_error_code(&error), "artifact_persist_failed");
    }

    #[test]
    fn local_artifact_cannot_be_overwritten_through_a_normalized_alias() {
        let workspace = tempfile::tempdir().expect("workspace");
        let backend = Arc::new(crate::workspace::LocalWorkspaceBackend::new(
            workspace.path(),
        ));
        let capture = capture_output("complete output");
        let artifact =
            persist_captured_text_artifact(backend.clone(), "task-7", "call-bash", capture.path())
                .expect("artifact");
        let alias = artifact.path.replacen(
            ".vv-agent/artifacts/",
            ".vv-agent/transient/../artifacts/",
            1,
        );

        let error = backend
            .write_text(&alias, "overwritten", false)
            .expect_err("normalized alias must remain immutable");
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(
            backend
                .read_text(&artifact.path)
                .expect("artifact contents"),
            "complete output"
        );
    }

    #[cfg(unix)]
    #[test]
    fn local_artifact_storage_isolated_from_workspace_symlinks() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::create_dir_all(workspace.path().join(".vv-agent/artifacts"))
            .expect("artifact root");
        symlink(
            outside.path(),
            workspace.path().join(".vv-agent/artifacts/task-7"),
        )
        .expect("task symlink");
        let backend = Arc::new(crate::workspace::LocalWorkspaceBackend::new(
            workspace.path(),
        ));

        let capture = capture_output("complete");
        let artifact =
            persist_captured_text_artifact(backend.clone(), "task-7", "call-bash", capture.path())
                .expect("private artifact write");
        assert_eq!(
            backend.read_text(&artifact.path).expect("private artifact"),
            "complete"
        );
        assert!(std::fs::read_dir(outside.path())
            .expect("outside directory")
            .next()
            .is_none());
    }

    #[test]
    fn capture_decoder_preserves_a_split_utf8_scalar_and_replaces_invalid_bytes() {
        use std::io::Write;

        let mut capture = tempfile::NamedTempFile::new().expect("capture file");
        capture
            .write_all(&[b'a'; CAPTURE_CHUNK_BYTES - 1])
            .expect("prefix");
        capture.write_all("你".as_bytes()).expect("scalar");
        capture.write_all(b"\xffdone").expect("invalid suffix");

        let output = read_captured_text_prefix(capture.path(), CAPTURE_CHUNK_BYTES + 8);
        assert!(output.ends_with("你\u{fffd}done"));
    }
}
