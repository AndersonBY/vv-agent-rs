use super::{background_session_manager, BackgroundSessionManager};

pub struct BackgroundSessionSubscription {
    session_id: String,
    listener_id: Option<u64>,
    manager: &'static BackgroundSessionManager,
}

impl BackgroundSessionSubscription {
    pub(in crate::runtime::background_sessions) fn new(
        session_id: String,
        listener_id: u64,
        manager: &'static BackgroundSessionManager,
    ) -> Self {
        Self {
            session_id,
            listener_id: Some(listener_id),
            manager,
        }
    }

    pub(in crate::runtime::background_sessions) fn noop() -> Self {
        Self {
            session_id: String::new(),
            listener_id: None,
            manager: background_session_manager(),
        }
    }

    pub fn unsubscribe(mut self) {
        if let Some(listener_id) = self.listener_id.take() {
            self.manager.unsubscribe(&self.session_id, listener_id);
        }
    }
}

impl Drop for BackgroundSessionSubscription {
    fn drop(&mut self) {
        if let Some(listener_id) = self.listener_id.take() {
            self.manager.unsubscribe(&self.session_id, listener_id);
        }
    }
}
