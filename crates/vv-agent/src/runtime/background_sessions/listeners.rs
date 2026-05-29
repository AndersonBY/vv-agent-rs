use std::sync::Arc;

use serde_json::Value;

pub type BackgroundSessionListener = Arc<dyn Fn(&Value) + Send + Sync + 'static>;

pub(in crate::runtime::background_sessions) fn notify_background_listeners(
    listeners: Vec<BackgroundSessionListener>,
    payload: &Value,
) {
    for listener in listeners {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            listener(payload);
        }));
    }
}
