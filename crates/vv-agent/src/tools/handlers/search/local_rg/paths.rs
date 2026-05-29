use std::path::Path;

use crate::tools::common::is_ignored_root;

pub(in crate::tools::handlers::search) fn is_workspace_root_path(path: &str) -> bool {
    let normalized = path.trim();
    normalized.is_empty() || normalized.replace('\\', "/") == "."
}

pub(super) fn local_ignored_root_names(base_path: &Path) -> Vec<String> {
    let mut roots = std::fs::read_dir(base_path)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            is_ignored_root(&name.to_ascii_lowercase()).then_some(name)
        })
        .collect::<Vec<_>>();
    roots.sort();
    roots
}
