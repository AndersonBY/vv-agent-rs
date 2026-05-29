use std::path::{Path, PathBuf};

use super::SessionMemory;

const SESSION_MEMORY_FILENAME: &str = "session_memory.json";

impl SessionMemory {
    pub fn storage_path(&self) -> Option<PathBuf> {
        let workspace = self.workspace.as_ref()?;
        let workspace_root = workspace.canonicalize().ok()?;
        let storage_dir = if self.config.storage_dir.is_absolute() {
            self.config.storage_dir.clone()
        } else {
            workspace.join(&self.config.storage_dir)
        };
        let scoped_dir = match self
            .storage_scope
            .as_deref()
            .filter(|scope| !scope.is_empty())
        {
            Some(scope) => storage_dir.join(sanitize_storage_scope(scope)?),
            None => storage_dir,
        };
        let resolved = scoped_dir.join(SESSION_MEMORY_FILENAME);
        let parent = resolved.parent()?;
        let canonical_parent = if parent.exists() {
            parent.canonicalize().ok()?
        } else {
            canonicalize_existing_ancestor(parent)?
        };
        if !canonical_parent.starts_with(&workspace_root) {
            return None;
        }
        Some(resolved)
    }
}

fn sanitize_storage_scope(raw: &str) -> Option<String> {
    let safe = raw
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches(['.', '_', '-'])
        .to_string();
    (!safe.is_empty()).then_some(safe)
}

fn canonicalize_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut current = path;
    while !current.exists() {
        current = current.parent()?;
    }
    current.canonicalize().ok()
}
