use std::sync::atomic::Ordering;

use crate::runtime::sub_agent_sessions::{SubAgentSessionListener, SubAgentSessionUnsubscribe};

use super::RuntimeSubAgentSession;

impl RuntimeSubAgentSession {
    pub(super) fn subscribe_listener(
        &self,
        listener: SubAgentSessionListener,
    ) -> SubAgentSessionUnsubscribe {
        let listener_id = self.next_listener_id.fetch_add(1, Ordering::Relaxed);
        self.listeners
            .lock()
            .expect("sub-agent session listeners poisoned")
            .insert(listener_id, listener);
        let listeners = self.listeners.clone();
        Box::new(move || {
            if let Ok(mut listeners) = listeners.lock() {
                listeners.remove(&listener_id);
            }
        })
    }
}
