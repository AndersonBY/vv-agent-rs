use std::path::Path;

pub(crate) fn is_hidden_path(path: &str) -> bool {
    path.split('/').any(|part| part.starts_with('.'))
}

pub(crate) fn workspace_relative_path_or_absolute(workspace: &Path, path: &Path) -> String {
    if path == workspace {
        return ".".to_string();
    }
    path.strip_prefix(workspace)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"))
}

pub(crate) fn collect_ignored_roots(files: &[String]) -> Vec<String> {
    let mut roots = files
        .iter()
        .filter_map(|path| path.split('/').next())
        .filter(|root| is_ignored_root(root))
        .map(str::to_string)
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    roots
}

pub(crate) fn is_ignored_root(root: &str) -> bool {
    matches!(
        root.to_ascii_lowercase().as_str(),
        ".venv"
            | "venv"
            | "node_modules"
            | ".git"
            | "__pycache__"
            | ".pytest_cache"
            | ".mypy_cache"
            | ".ruff_cache"
            | ".idea"
            | ".vscode"
            | "dist"
            | "build"
            | ".next"
            | ".nuxt"
            | ".cache"
            | "target"
            | "vendor"
    )
}
