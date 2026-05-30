use std::path::PathBuf;

pub(super) fn resolve_rg_executable() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(if cfg!(windows) { "rg.exe" } else { "rg" });
        if candidate.is_file() {
            return Some(candidate);
        }
        if cfg!(windows) {
            let candidate = directory.join("rg.cmd");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
