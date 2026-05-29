use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::json;

use super::listeners::{notify_background_listeners, BackgroundSessionListener};

#[test]
fn notify_background_listeners_continues_after_listener_panic() {
    let delivered = Arc::new(AtomicUsize::new(0));
    let delivered_listener = Arc::clone(&delivered);
    let listeners: Vec<BackgroundSessionListener> = vec![
        Arc::new(|_| panic!("boom")),
        Arc::new(move |_| {
            delivered_listener.fetch_add(1, Ordering::Relaxed);
        }),
    ];

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        notify_background_listeners(listeners, &json!({"status": "completed"}));
    }));

    assert!(result.is_ok());
    assert_eq!(delivered.load(Ordering::Relaxed), 1);
}
