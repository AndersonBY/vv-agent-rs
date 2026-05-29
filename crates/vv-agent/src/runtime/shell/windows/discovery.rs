use std::path::Path;

use super::super::path::find_executable_in_path;

pub(super) trait WindowsShellDiscovery {
    fn env_var(&self, key: &str) -> Option<String>;
    fn which(&self, program: &str) -> Option<String>;
    fn is_file(&self, path: &Path) -> bool;
}

pub(super) struct RealWindowsShellDiscovery;

impl WindowsShellDiscovery for RealWindowsShellDiscovery {
    fn env_var(&self, key: &str) -> Option<String> {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    fn which(&self, program: &str) -> Option<String> {
        find_executable_in_path(program).map(|path| path.to_string_lossy().to_string())
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
}
