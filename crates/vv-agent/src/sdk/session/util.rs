use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SESSION_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(super) fn normalize_session_prompt(prompt: String, label: &str) -> Result<String, String> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err(format!("{label} cannot be empty"));
    }
    Ok(prompt.to_string())
}

pub(super) fn absolutize_workspace(path: PathBuf) -> PathBuf {
    let path = expand_user_path(path);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|current_dir| current_dir.join(&path))
            .unwrap_or(path)
    };
    path.canonicalize().unwrap_or(path)
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

pub(super) fn generate_session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let counter = SESSION_ID_COUNTER.fetch_add(1, Ordering::Relaxed) as u128;
    format!("{:012x}", (nanos ^ counter) & 0xffff_ffff_ffff)
}

pub(super) fn normalize_session_id(session_id: impl Into<String>) -> String {
    let session_id = session_id.into().trim().to_string();
    if session_id.is_empty() {
        generate_session_id()
    } else {
        session_id
    }
}
