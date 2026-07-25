use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::workspace::read_captured_text_prefix;

static PROCESS_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(super) fn next_output_path() -> PathBuf {
    let counter = PROCESS_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "vv_agent_process_{}_{}.log",
        std::process::id(),
        counter
    ))
}

pub(super) fn open_output_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .read(true)
        .open(path)
}

pub fn read_captured_output(path: &Path, limit_chars: usize) -> String {
    read_captured_text_prefix(path, limit_chars)
}

pub fn remove_captured_output(path: &Path) {
    let _ = fs::remove_file(path);
}
