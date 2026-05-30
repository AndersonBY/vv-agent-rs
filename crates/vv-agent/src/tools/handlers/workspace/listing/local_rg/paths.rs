use std::path::Path;

use crate::tools::common::is_ignored_root;

pub(super) fn normalize_rg_relative_path(path: std::borrow::Cow<'_, str>) -> String {
    let normalized = path.replace('\\', "/");
    normalized
        .strip_prefix("./")
        .unwrap_or(&normalized)
        .trim_start_matches('/')
        .to_string()
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
            is_ignored_root(&name).then_some(name)
        })
        .collect::<Vec<_>>();
    roots.sort();
    roots
}
