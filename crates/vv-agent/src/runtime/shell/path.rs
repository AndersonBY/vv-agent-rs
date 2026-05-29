use std::path::{Path, PathBuf};

pub(super) fn looks_like_path(value: &str) -> bool {
    value.contains('/') || value.contains('\\') || value.contains(':')
}

pub(super) fn expand_home_path(value: &str) -> PathBuf {
    if value == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    } else if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

pub(super) fn find_executable_in_path(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if looks_like_path(program) {
        return path.is_file().then(|| path.to_path_buf());
    }

    let path_var = std::env::var_os("PATH")?;
    let extensions = executable_extensions(program);
    for directory in std::env::split_paths(&path_var) {
        for extension in &extensions {
            let candidate = directory.join(format!("{program}{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn executable_extensions(program: &str) -> Vec<String> {
    if !cfg!(target_os = "windows") || Path::new(program).extension().is_some() {
        return vec![String::new()];
    }
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    let mut extensions = vec![String::new()];
    for extension in pathext.split(';') {
        let extension = extension.trim();
        if extension.is_empty() {
            continue;
        }
        let normalized = if extension.starts_with('.') {
            extension.to_string()
        } else {
            format!(".{extension}")
        };
        if !extensions.iter().any(|seen| seen == &normalized) {
            extensions.push(normalized);
        }
    }
    extensions
}
