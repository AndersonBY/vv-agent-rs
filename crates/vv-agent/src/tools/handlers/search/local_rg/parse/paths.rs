pub(super) fn normalize_rg_relative_path(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    while normalized.starts_with("./") {
        normalized = normalized[2..].to_string();
    }
    if normalized == "." {
        String::new()
    } else {
        normalized
    }
}
