use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::runtime::CancellationToken;

use super::events::emit_session_event;
use super::state::{SessionEventHandler, SessionListenerId};
use super::util::normalize_session_prompt;

#[derive(Clone)]
pub struct SessionSteeringHandle {
    pub(super) steering_queue: Arc<Mutex<VecDeque<String>>>,
    pub(super) listeners: Arc<Mutex<BTreeMap<SessionListenerId, SessionEventHandler>>>,
}

impl SessionSteeringHandle {
    pub fn steer(&self, prompt: impl Into<String>) -> Result<(), String> {
        let prompt = normalize_session_prompt(prompt.into(), "steer prompt")?;
        {
            let mut queue = self
                .steering_queue
                .lock()
                .map_err(|_| "Session steering queue lock is poisoned.".to_string())?;
            queue.push_back(prompt.clone());
        }
        emit_session_event(
            &self.listeners,
            "session_steer_queued",
            BTreeMap::from([("prompt".to_string(), Value::String(prompt))]),
        );
        Ok(())
    }
}

#[derive(Clone)]
pub struct SessionCancellationHandle {
    pub(super) active_cancellation_token: Arc<Mutex<Option<CancellationToken>>>,
    pub(super) steering_queue: Arc<Mutex<VecDeque<String>>>,
    pub(super) follow_up_queue: Arc<Mutex<VecDeque<String>>>,
    pub(super) listeners: Arc<Mutex<BTreeMap<SessionListenerId, SessionEventHandler>>>,
}

impl SessionCancellationHandle {
    pub fn cancel(&self) -> bool {
        let token = {
            self.active_cancellation_token
                .lock()
                .expect("session cancellation token lock")
                .clone()
        };
        let Some(token) = token else {
            return false;
        };
        token.cancel();
        if let Ok(mut queue) = self.steering_queue.lock() {
            queue.clear();
        }
        if let Ok(mut queue) = self.follow_up_queue.lock() {
            queue.clear();
        }
        emit_session_event(&self.listeners, "session_cancel_requested", BTreeMap::new());
        true
    }
}
