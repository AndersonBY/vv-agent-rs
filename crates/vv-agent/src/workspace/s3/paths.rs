use object_store::path::Path as ObjectPath;

use crate::workspace::normalize_workspace_path;

pub(super) fn object_key(prefix: &str, path: &str) -> String {
    let path = normalize_workspace_path(path);
    match (prefix.is_empty(), path.is_empty()) {
        (true, _) => path,
        (false, true) => prefix.to_string(),
        (false, false) => format!("{prefix}/{path}"),
    }
}

pub(super) fn relative_key(prefix: &str, key: &str) -> Option<String> {
    if prefix.is_empty() {
        return Some(normalize_workspace_path(key));
    }
    if key == prefix {
        return Some(String::new());
    }
    key.strip_prefix(&format!("{prefix}/"))
        .map(normalize_workspace_path)
}

pub(super) fn list_prefix(prefix: &str) -> Option<ObjectPath> {
    if prefix.is_empty() {
        None
    } else {
        Some(ObjectPath::from(prefix.to_string()))
    }
}
