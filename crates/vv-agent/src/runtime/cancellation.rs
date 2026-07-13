use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

thread_local! {
    static CANCELLATION_SCOPES: RefCell<Vec<CancellationToken>> = const { RefCell::new(Vec::new()) };
}

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
    reason: Mutex<Option<String>>,
    callbacks: Mutex<Vec<Arc<dyn Fn() + Send + Sync + 'static>>>,
}

fn invoke_callback(callback: &Arc<dyn Fn() + Send + Sync + 'static>) {
    let _ = catch_unwind(AssertUnwindSafe(|| callback()));
}

pub(crate) struct CancellationScope {
    active: bool,
}

impl Drop for CancellationScope {
    fn drop(&mut self) {
        if self.active {
            CANCELLATION_SCOPES.with(|scopes| {
                scopes.borrow_mut().pop();
            });
        }
    }
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
        self.cancel_with_reason("Operation was cancelled");
    }

    pub fn cancel_with_reason(&self, reason: impl Into<String>) {
        let reason = reason.into();
        let reason = if reason.trim().is_empty() {
            "Operation was cancelled".to_string()
        } else {
            reason
        };
        let mut stored_reason = self
            .inner
            .reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.inner.cancelled.load(Ordering::SeqCst) {
            return;
        }
        *stored_reason = Some(reason);
        self.inner.cancelled.store(true, Ordering::SeqCst);
        drop(stored_reason);
        let callbacks = std::mem::take(
            &mut *self
                .inner
                .callbacks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for callback in callbacks {
            invoke_callback(&callback);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    pub fn cancelled(&self) -> bool {
        self.is_cancelled()
    }

    pub fn reason(&self) -> Option<String> {
        self.inner
            .reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn check(&self) -> Result<(), CancelledError> {
        if self.is_cancelled() {
            Err(CancelledError::new(
                self.reason()
                    .unwrap_or_else(|| "Operation was cancelled".to_string()),
            ))
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
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.is_cancelled() {
                true
            } else {
                callbacks.push(callback.clone());
                false
            }
        };
        if call_immediately {
            invoke_callback(&callback);
        }
    }

    pub fn child(&self) -> Self {
        let child = Self::default();
        let child_to_cancel = child.clone();
        let parent = Arc::downgrade(&self.inner);
        self.on_cancel(move || {
            let reason = parent
                .upgrade()
                .and_then(|inner| {
                    inner
                        .reason
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone()
                })
                .unwrap_or_else(|| "Operation was cancelled".to_string());
            child_to_cancel.cancel_with_reason(reason);
        });
        child
    }

    pub(crate) fn enter_scope(token: Option<&Self>) -> CancellationScope {
        let active = if let Some(token) = token {
            CANCELLATION_SCOPES.with(|scopes| scopes.borrow_mut().push(token.clone()));
            true
        } else {
            false
        };
        CancellationScope { active }
    }

    pub(crate) fn child_of_current() -> Option<Self> {
        CANCELLATION_SCOPES.with(|scopes| scopes.borrow().last().map(Self::child))
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::CancellationToken;

    #[test]
    fn cancel_is_idempotent_and_callbacks_run_once() {
        let token = CancellationToken::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_callback = calls.clone();
        token.on_cancel(move || {
            calls_for_callback.fetch_add(1, Ordering::SeqCst);
        });

        token.cancel();
        token.cancel();

        assert!(token.is_cancelled());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cancel_isolates_callback_panics_and_still_cancels_children() {
        let token = CancellationToken::default();
        let calls = Arc::new(AtomicUsize::new(0));
        token.on_cancel(|| panic!("callback panic"));
        let calls_for_callback = calls.clone();
        token.on_cancel(move || {
            calls_for_callback.fetch_add(1, Ordering::SeqCst);
        });
        let child = token.child();

        let outcome = catch_unwind(AssertUnwindSafe(|| token.cancel()));

        assert!(outcome.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(child.is_cancelled());
    }

    #[test]
    fn callbacks_registered_after_cancellation_are_individually_isolated() {
        let token = CancellationToken::default();
        token.cancel();

        let panic_outcome = catch_unwind(AssertUnwindSafe(|| {
            token.on_cancel(|| panic!("late callback panic"));
        }));
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_callback = calls.clone();
        token.on_cancel(move || {
            calls_for_callback.fetch_add(1, Ordering::SeqCst);
        });

        assert!(panic_outcome.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn current_scope_derives_one_way_child_cancellation() {
        let parent = CancellationToken::default();
        let child = {
            let _scope = CancellationToken::enter_scope(Some(&parent));
            CancellationToken::child_of_current().expect("derived child")
        };

        child.cancel();
        assert!(!parent.is_cancelled());

        let second_child = {
            let _scope = CancellationToken::enter_scope(Some(&parent));
            CancellationToken::child_of_current().expect("derived child")
        };
        parent.cancel();
        assert!(second_child.is_cancelled());
    }

    #[test]
    fn child_preserves_parent_cancellation_reason() {
        let parent = CancellationToken::default();
        let child = parent.child();

        parent.cancel_with_reason("host requested cancellation");

        assert_eq!(
            child.reason().as_deref(),
            Some("host requested cancellation")
        );
        assert_eq!(
            child.check().expect_err("child cancellation").message(),
            "host requested cancellation"
        );
    }
}
