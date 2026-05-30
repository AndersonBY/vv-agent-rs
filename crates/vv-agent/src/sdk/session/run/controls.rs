use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::sdk::types::AgentRun;

use super::super::handles::{SessionCancellationHandle, SessionSteeringHandle};
use super::super::util::normalize_session_prompt;
use super::super::AgentSession;

impl AgentSession {
    pub fn follow_up(&mut self, prompt: impl Into<String>) -> Result<(), String> {
        let prompt = normalize_session_prompt(prompt.into(), "follow_up prompt")?;
        self.follow_up_queue
            .lock()
            .map_err(|_| "Session follow-up queue lock is poisoned.".to_string())?
            .push_back(prompt.clone());
        self.emit(
            "session_follow_up_queued",
            BTreeMap::from([("prompt".to_string(), Value::String(prompt))]),
        );
        Ok(())
    }

    pub fn steer(&mut self, prompt: impl Into<String>) -> Result<(), String> {
        self.steering_handle().steer(prompt)
    }

    pub fn steering_handle(&self) -> SessionSteeringHandle {
        SessionSteeringHandle {
            steering_queue: Arc::clone(&self.steering_queue),
            listeners: Arc::clone(&self.listeners),
        }
    }

    pub fn cancellation_handle(&self) -> SessionCancellationHandle {
        SessionCancellationHandle {
            active_cancellation_token: Arc::clone(&self.active_cancellation_token),
            steering_queue: Arc::clone(&self.steering_queue),
            follow_up_queue: Arc::clone(&self.follow_up_queue),
            listeners: Arc::clone(&self.listeners),
        }
    }

    pub fn cancel(&self) -> bool {
        self.cancellation_handle().cancel()
    }

    pub fn clear_queues(&mut self) {
        if let Ok(mut queue) = self.steering_queue.lock() {
            queue.clear();
        }
        if let Ok(mut queue) = self.follow_up_queue.lock() {
            queue.clear();
        }
        self.emit("session_queues_cleared", BTreeMap::new());
    }

    pub fn continue_run(&mut self, prompt: Option<String>) -> Result<AgentRun, String> {
        if let Some(prompt) = prompt {
            let prompt = prompt.trim();
            if !prompt.is_empty() {
                return self.prompt_with_auto_follow_up(prompt.to_string(), false);
            }
        }

        let queued_prompt = {
            let mut steering_queue = self
                .steering_queue
                .lock()
                .map_err(|_| "Session steering queue lock is poisoned.".to_string())?;
            steering_queue.pop_front()
        }
        .or_else(|| {
            self.follow_up_queue
                .lock()
                .expect("session follow-up queue lock")
                .pop_front()
        })
        .ok_or_else(|| {
            "No queued prompt available. Provide prompt or call steer()/follow_up() first."
                .to_string()
        })?;
        self.prompt_with_auto_follow_up(queued_prompt, false)
    }
}
