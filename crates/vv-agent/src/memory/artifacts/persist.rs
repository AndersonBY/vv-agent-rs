use std::path::{Path, PathBuf};

use super::ToolResultArtifactConfig;

pub(super) fn persist_tool_content(
    content: &str,
    tool_call_id: Option<&str>,
    config: &ToolResultArtifactConfig,
    cycle_index: Option<u32>,
) -> Option<String> {
    let workspace = config.workspace.as_ref()?;
    let artifact_path = build_tool_artifact_path(tool_call_id, &config.artifact_dir, cycle_index);
    if artifact_path.is_absolute() {
        return None;
    }
    std::fs::create_dir_all(workspace).ok()?;
    let workspace_root = workspace.canonicalize().ok()?;
    let target = workspace_root.join(&artifact_path);
    if let Some(parent) = target.parent() {
        if !canonicalize_existing_ancestor(parent)?.starts_with(&workspace_root) {
            return None;
        }
        std::fs::create_dir_all(parent).ok()?;
        if !parent.canonicalize().ok()?.starts_with(&workspace_root) {
            return None;
        }
    }
    if std::fs::symlink_metadata(&target).is_ok()
        && !target.canonicalize().ok()?.starts_with(&workspace_root)
    {
        return None;
    }
    std::fs::write(&target, content).ok()?;
    Some(path_to_string(&artifact_path))
}

fn build_tool_artifact_path(
    tool_call_id: Option<&str>,
    artifact_dir: &Path,
    cycle_index: Option<u32>,
) -> PathBuf {
    let safe_id = sanitize_tool_call_id(tool_call_id.unwrap_or_default())
        .unwrap_or_else(|| format!("tool_result_{}", uuid::Uuid::new_v4().simple()));
    let filename = format!("{safe_id}.txt");
    match cycle_index {
        Some(cycle_index) => artifact_dir
            .join(format!("cycle_{cycle_index}"))
            .join(filename),
        None => artifact_dir.join(filename),
    }
}

fn sanitize_tool_call_id(value: &str) -> Option<String> {
    let safe = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    safe.chars()
        .any(|ch| ch.is_ascii_alphanumeric())
        .then_some(safe)
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn canonicalize_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut current = path;
    while !current.exists() {
        current = current.parent()?;
    }
    current.canonicalize().ok()
}
