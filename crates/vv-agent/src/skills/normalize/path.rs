use std::path::{Path, PathBuf};

pub(super) fn path_exists(raw_path: &str, workspace: Option<&Path>) -> bool {
    let path = expand_user(raw_path);
    if path.exists() {
        return true;
    }
    if let Some(workspace) = workspace {
        if !path.is_absolute() {
            return workspace.join(&path).exists();
        }
    }
    false
}

pub(super) fn resolve_skill_path(raw_path: &str, workspace: Option<&Path>) -> PathBuf {
    let path = expand_user(raw_path);
    let path = if path.is_absolute() {
        path
    } else if let Some(workspace) = workspace {
        workspace.join(path)
    } else {
        path
    };
    let path = path.canonicalize().unwrap_or(path);
    if path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("skill.md"))
    {
        return path.parent().map(Path::to_path_buf).unwrap_or(path);
    }
    path
}

pub(super) fn relative_location(skill_md: &Path, workspace: Option<&Path>) -> String {
    let normalized_skill_md = skill_md
        .canonicalize()
        .unwrap_or_else(|_| skill_md.to_path_buf());
    if let Some(workspace) = workspace {
        let normalized_workspace = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf());
        if let Ok(relative) = normalized_skill_md.strip_prefix(normalized_workspace) {
            return relative.to_string_lossy().replace('\\', "/");
        }
    }
    normalized_skill_md.to_string_lossy().replace('\\', "/")
}

fn expand_user(raw_path: &str) -> PathBuf {
    if let Some(rest) = raw_path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw_path)
}
