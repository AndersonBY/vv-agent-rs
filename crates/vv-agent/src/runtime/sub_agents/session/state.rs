use std::collections::BTreeMap;

use serde_json::Value;

use super::RuntimeSubAgentSession;

impl RuntimeSubAgentSession {
    pub(super) fn queue_steering(&self, prompt: &str) -> Result<(), String> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err("Steering prompt cannot be empty.".to_string());
        }
        self.steering_queue
            .lock()
            .map_err(|_| "Sub-agent steering queue lock is poisoned.".to_string())?
            .push_back(prompt.to_string());
        self.emit(
            "session_steer_queued",
            BTreeMap::from([("prompt".to_string(), Value::String(prompt.to_string()))]),
        );
        Ok(())
    }

    pub(super) fn sanitize_state_for_resume(&self) -> usize {
        let Ok(mut state) = self.state.lock() else {
            return 0;
        };
        let sanitized = crate::memory::sanitize_for_resume(&state.messages);
        let removed = state.messages.len().saturating_sub(sanitized.len());
        state.messages = sanitized;
        removed
    }
}
