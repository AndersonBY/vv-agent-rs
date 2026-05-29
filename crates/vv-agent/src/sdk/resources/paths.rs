use std::path::{Path, PathBuf};

use serde_json::Value;

pub(super) fn read_resolved_path_list(
    payload: &serde_json::Map<String, Value>,
    key: &str,
    base_dir: &Path,
) -> Vec<String> {
    payload
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|path| resolve_resource_path(base_dir, path))
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn resolve_existing_or_absolute_path(path: PathBuf) -> PathBuf {
    let path = absolutize_path(expand_user_path(path));
    path.canonicalize().unwrap_or(path)
}

fn resolve_resource_path(base_dir: &Path, raw_path: &str) -> String {
    let path = expand_user_path(PathBuf::from(raw_path));
    let path = if path.is_absolute() {
        path
    } else {
        absolutize_path(base_dir.join(path))
    };
    let path = path.canonicalize().unwrap_or(path);
    path.to_string_lossy().to_string()
}

fn expand_user_path(path: PathBuf) -> PathBuf {
    let raw_path = path.to_string_lossy();
    if raw_path == "~" {
        return home_dir().unwrap_or(path);
    }
    if let Some(rest) = raw_path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    path
}

fn absolutize_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir()
        .map(|current_dir| current_dir.join(&path))
        .unwrap_or(path)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            let mut home = PathBuf::from(drive);
            home.push(path);
            Some(home)
        })
}
