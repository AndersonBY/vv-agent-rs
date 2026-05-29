use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::state::{SessionEventHandler, SessionListenerId};
use super::AgentSession;

pub(super) fn emit_session_event(
    listeners: &Arc<Mutex<BTreeMap<SessionListenerId, SessionEventHandler>>>,
    event: &str,
    payload: BTreeMap<String, Value>,
) {
    let listeners: Vec<SessionEventHandler> = listeners
        .lock()
        .expect("session listeners lock")
        .values()
        .cloned()
        .collect();
    for listener in listeners {
        listener(event, &payload);
    }
}

impl AgentSession {
    pub fn subscribe(&mut self, listener: SessionEventHandler) -> SessionListenerId {
        let listener_id = self.next_listener_id;
        self.next_listener_id = self.next_listener_id.saturating_add(1);
        self.listeners
            .lock()
            .expect("session listeners lock")
            .insert(listener_id, listener);
        listener_id
    }

    pub fn unsubscribe(&mut self, listener_id: SessionListenerId) -> bool {
        self.listeners
            .lock()
            .expect("session listeners lock")
            .remove(&listener_id)
            .is_some()
    }

    pub(super) fn emit(&self, event: &str, payload: BTreeMap<String, Value>) {
        emit_session_event(&self.listeners, event, payload);
    }
}
