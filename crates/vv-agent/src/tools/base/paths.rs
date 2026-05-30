use std::path::{Path, PathBuf};

pub(crate) fn resolve_workspace_path_checked(
    workspace: &Path,
    raw_path: &str,
    allow_outside_workspace_paths: bool,
) -> Result<PathBuf, String> {
    let base = workspace
        .canonicalize()
        .unwrap_or_else(|_| absolutize_without_canonicalizing(workspace));
    let candidate = crate::workspace::expand_home_path(raw_path);
    let target = if candidate.is_absolute() {
        absolutize_without_canonicalizing(&candidate)
    } else {
        absolutize_without_canonicalizing(&base.join(&candidate))
    };
    let normalized = normalize_path(target);
    if !allow_outside_workspace_paths && normalized != base && !normalized.starts_with(&base) {
        return Err(format!("Path escapes workspace: {raw_path}"));
    }
    Ok(normalized)
}

fn absolutize_without_canonicalizing(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn normalize_path(path: PathBuf) -> PathBuf {
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
