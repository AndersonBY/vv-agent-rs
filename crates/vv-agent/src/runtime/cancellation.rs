use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelledError {
    message: String,
}

impl CancelledError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for CancelledError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CancelledError {}

#[derive(Clone, Default)]
pub struct CancellationToken {
    inner: Arc<CancellationState>,
}

#[derive(Default)]
struct CancellationState {
    cancelled: AtomicBool,
    callbacks: Mutex<Vec<Arc<dyn Fn() + Send + Sync + 'static>>>,
}

impl std::fmt::Debug for CancellationToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl CancellationToken {
    pub fn cancel(&self) {
        if self.inner.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        let callbacks = std::mem::take(
            &mut *self
                .inner
                .callbacks
                .lock()
                .expect("cancellation callbacks lock"),
        );
        for callback in callbacks {
            callback();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    pub fn check(&self) -> Result<(), String> {
        if self.is_cancelled() {
            Err("Operation was cancelled".to_string())
        } else {
            Ok(())
        }
    }

    pub fn on_cancel(&self, callback: impl Fn() + Send + Sync + 'static) {
        let callback: Arc<dyn Fn() + Send + Sync + 'static> = Arc::new(callback);
        let call_immediately = {
            let mut callbacks = self
                .inner
                .callbacks
                .lock()
                .expect("cancellation callbacks lock");
            if self.is_cancelled() {
                true
            } else {
                callbacks.push(callback.clone());
                false
            }
        };
        if call_immediately {
            callback();
        }
    }

    pub fn child(&self) -> Self {
        let child = Self::default();
        let child_to_cancel = child.clone();
        self.on_cancel(move || child_to_cancel.cancel());
        child
    }
}
