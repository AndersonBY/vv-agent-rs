use std::sync::atomic::{AtomicU64, Ordering};

static SDK_TASK_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(in crate::sdk::client) fn generate_task_id(prefix: &str) -> String {
    let normalized_prefix = prefix.trim();
    let prefix = if normalized_prefix.is_empty() {
        "inline"
    } else {
        normalized_prefix
    };
    let counter = SDK_TASK_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    format!("{prefix}_{:08x}", counter & 0xffff_ffff)
}
