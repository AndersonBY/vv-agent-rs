use std::io::{Error, ErrorKind};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::types::ToolArtifactRef;

use super::WorkspaceBackend;

pub(crate) const BOUNDED_TEXT_CHARS: usize = 12_000;
const PREVIEW_HEAD_CHARS: usize = 6_000;
const PREVIEW_TAIL_CHARS: usize = 5_953;
const PREVIEW_MARKER: &str = "\n... output omitted; full text in artifact ...\n";

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

pub(crate) fn persist_text_artifact(
    backend: Arc<dyn WorkspaceBackend>,
    task_id: &str,
    tool_call_id: &str,
    text: &str,
) -> std::io::Result<ToolArtifactRef> {
    let task = artifact_segment(task_id, "task");
    let call = artifact_segment(tool_call_id, "call");
    let bytes = text.as_bytes();
    let sha256 = format!("{:x}", Sha256::digest(bytes));
    let mut last_collision = None;
    for _ in 0..32 {
        let suffix = Uuid::new_v4().simple().to_string();
        let path = format!(".vv-agent/artifacts/{task}/{call}-{suffix}.txt");
        match backend.write_text_exclusive(&path, text) {
            Ok(written) if written == bytes.len() => {
                return Ok(ToolArtifactRef {
                    path,
                    media_type: "text/plain".to_string(),
                    encoding: "utf-8".to_string(),
                    size_bytes: bytes.len() as u64,
                    sha256,
                });
            }
            Ok(written) => {
                return Err(Error::new(
                    ErrorKind::WriteZero,
                    format!("artifact write reported {written} of {} bytes", bytes.len()),
                ));
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
            self.inner.write_text_exclusive(path, content)
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

    #[test]
    fn artifact_collision_selects_a_new_exclusive_path() {
        let backend = Arc::new(ExclusiveProbeBackend::new(1, None));
        let artifact = persist_text_artifact(backend.clone(), "task-7", "call-bash", "complete")
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
        let error = persist_text_artifact(backend, "task-7", "call-bash", "complete")
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
        let artifact =
            persist_text_artifact(backend.clone(), "task-7", "call-bash", "complete output")
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
    fn local_artifact_write_rejects_a_symlink_segment() {
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

        let error = persist_text_artifact(backend, "task-7", "call-bash", "complete")
            .expect_err("symlink traversal must fail");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(artifact_write_error_code(&error), "artifact_path_invalid");
        assert!(std::fs::read_dir(outside.path())
            .expect("outside directory")
            .next()
            .is_none());
    }
}
