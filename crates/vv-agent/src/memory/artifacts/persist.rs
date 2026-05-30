use std::path::{Path, PathBuf};

use super::ToolResultArtifactConfig;

pub(super) fn persist_tool_content(
    content: &str,
    tool_call_id: Option<&str>,
    config: &ToolResultArtifactConfig,
) -> Option<String> {
    let workspace = config.workspace.as_ref()?;
    let artifact_path = build_tool_artifact_path(tool_call_id, &config.artifact_dir);
    let target = workspace.join(&artifact_path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    std::fs::write(&target, content).ok()?;
    Some(path_to_string(&artifact_path))
}

fn build_tool_artifact_path(tool_call_id: Option<&str>, artifact_dir: &Path) -> PathBuf {
    let safe_id = sanitize_tool_call_id(tool_call_id.unwrap_or("tool_result"));
    artifact_dir.join(format!("{safe_id}.txt"))
}

fn sanitize_tool_call_id(value: &str) -> String {
    let safe = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if safe.trim().is_empty() {
        "tool_result".to_string()
    } else {
        safe
    }
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
