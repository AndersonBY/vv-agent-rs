use sha2::{Digest, Sha256};

use super::{
    DiscoveryFilteredWorkspaceBackend, LocalWorkspaceBackend, MemoryWorkspaceBackend,
    S3WorkspaceBackend, WorkspaceBackend,
};

pub(crate) struct TextScan {
    pub size_bytes: u64,
    pub sha256: String,
    pub valid_utf8: bool,
}

fn read_chunks(
    backend: &dyn WorkspaceBackend,
    path: &str,
    consume: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let native = backend.as_any();
    if let Some(local) = native.downcast_ref::<LocalWorkspaceBackend>() {
        local.read_chunks(path, consume)
    } else if let Some(memory) = native.downcast_ref::<MemoryWorkspaceBackend>() {
        memory.read_chunks(path, consume)
    } else if let Some(s3) = native.downcast_ref::<S3WorkspaceBackend>() {
        s3.read_chunks(path, consume)
    } else if let Some(filtered) = native.downcast_ref::<DiscoveryFilteredWorkspaceBackend>() {
        read_chunks(filtered.inner().as_ref(), path, consume)
    } else {
        // Custom backends retain their existing read contract.
        consume(&backend.read_bytes(path)?)
    }
}

pub(crate) fn scan_text(
    backend: &dyn WorkspaceBackend,
    path: &str,
    mut consume: impl FnMut(&str),
) -> std::io::Result<TextScan> {
    let mut digest = Sha256::new();
    let mut size_bytes = 0;
    let mut pending = Vec::new();
    let mut valid_utf8 = true;
    read_chunks(backend, path, &mut |chunk| {
        digest.update(chunk);
        size_bytes += chunk.len() as u64;
        if valid_utf8 {
            pending.extend_from_slice(chunk);
            match std::str::from_utf8(&pending) {
                Ok(text) => {
                    consume(text);
                    pending.clear();
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    consume(std::str::from_utf8(&pending[..valid]).expect("validated prefix"));
                    valid_utf8 = error.error_len().is_none();
                    pending.drain(..valid);
                }
            }
        }
        Ok(())
    })?;
    valid_utf8 &= pending.is_empty();
    Ok(TextScan {
        size_bytes,
        sha256: format!("{:x}", digest.finalize()),
        valid_utf8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{build_default_registry, ToolContext};
    use crate::types::{ToolCall, ToolResultStatus};
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn native_backends_scan_utf8_across_chunks_and_reject_changed_tail() {
        let workspace = tempfile::tempdir().unwrap();
        let backends: Vec<Arc<dyn WorkspaceBackend>> = vec![
            Arc::new(LocalWorkspaceBackend::new(workspace.path())),
            Arc::new(MemoryWorkspaceBackend::default()),
            Arc::new(S3WorkspaceBackend::default()),
        ];
        let text = format!(
            "\u{feff}{}中\r\n{}",
            "x".repeat(65533),
            "🙂".repeat(100_000)
        );
        for backend in backends {
            let filtered = Arc::new(
                DiscoveryFilteredWorkspaceBackend::new(backend.clone(), "hidden").unwrap(),
            );
            backend.write_text("large.txt", &text, false).unwrap();
            let mut context = ToolContext::new(workspace.path());
            context.workspace_backend = filtered;
            let registry = build_default_registry();
            let first = registry
                .execute(
                    &ToolCall::new(
                        "first",
                        "read_file",
                        std::collections::BTreeMap::from([
                            ("path".into(), json!("large.txt")),
                            ("show_line_numbers".into(), json!(true)),
                        ]),
                    ),
                    &mut context,
                )
                .unwrap();
            assert_eq!(first.status, ToolResultStatus::Success);
            assert_eq!(first.content, format!("1: {}", "x".repeat(11997)));
            assert!(first.truncated);
            backend
                .write_text("large.txt", "changed tail", true)
                .unwrap();
            let stale = registry
                .execute(
                    &ToolCall::new(
                        "stale",
                        "read_file",
                        std::collections::BTreeMap::from([
                            ("path".into(), json!("large.txt")),
                            ("cursor".into(), json!(first.cursor.unwrap())),
                        ]),
                    ),
                    &mut context,
                )
                .unwrap();
            assert_eq!(stale.error_code.as_deref(), Some("stale_cursor"));
        }
    }
}
