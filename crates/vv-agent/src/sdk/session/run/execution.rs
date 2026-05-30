use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::runtime::CancellationToken;
use crate::sdk::types::{agent_status_value, AgentRun};

use super::super::events::emit_session_event;
use super::super::state::{AgentSessionRunRequest, SessionEventHandler};
use super::super::watchers::sync_background_command_watchers;
use super::super::AgentSession;

impl AgentSession {
    pub(super) fn run_once(&mut self, prompt: String) -> Result<AgentRun, String> {
        if self.running {
            return Err(
                "Session is already running. Queue with steer()/follow_up() or wait for completion."
                    .to_string(),
            );
        }
        let existing_messages = self.messages.len();
        self.running = true;
        let cancellation_token = CancellationToken::default();
        *self
            .active_cancellation_token
            .lock()
            .map_err(|_| "Session cancellation token lock is poisoned.".to_string())? =
            Some(cancellation_token.clone());
        self.emit(
            "session_run_start",
            BTreeMap::from([
                ("prompt".to_string(), Value::String(prompt.clone())),
                (
                    "existing_messages".to_string(),
                    Value::from(existing_messages as u64),
                ),
            ]),
        );
        let listeners = Arc::clone(&self.listeners);
        let background_command_subscriptions = Arc::clone(&self.background_command_subscriptions);
        let steering_handle = self.steering_handle();
        let runtime_event_handler: SessionEventHandler = Arc::new(move |event, payload| {
            sync_background_command_watchers(
                &background_command_subscriptions,
                &listeners,
                &steering_handle,
                event,
                payload,
            );
            emit_session_event(&listeners, event, payload.clone());
        });
        let run = (self.execute_run)(AgentSessionRunRequest {
            prompt,
            task_name: Some(self._agent_name.clone()),
            workspace: Some(self.workspace.clone()),
            initial_messages: self.messages.clone(),
            shared_state: self.shared_state.clone(),
            metadata: BTreeMap::from([(
                "session_id".to_string(),
                Value::String(self.session_id.clone()),
            )]),
            runtime_event_handler: Some(runtime_event_handler),
            before_cycle_messages: None,
            interruption_messages: None,
            steering_queue: Some(Arc::clone(&self.steering_queue)),
            cancellation_token: Some(cancellation_token),
            stream_callback: None,
            sub_task_manager: Some(self.sub_task_manager.clone()),
        });
        self.running = false;
        *self
            .active_cancellation_token
            .lock()
            .map_err(|_| "Session cancellation token lock is poisoned.".to_string())? = None;
        let run = run?;
        self.messages = run.result.messages.clone();
        self.shared_state = run.result.shared_state.clone();
        self.latest_run = Some(run.clone());
        self.emit(
            "session_run_end",
            BTreeMap::from([
                (
                    "status".to_string(),
                    Value::String(agent_status_value(run.result.status).to_string()),
                ),
                (
                    "cycles".to_string(),
                    Value::from(run.result.cycles.len() as u64),
                ),
                (
                    "final_answer".to_string(),
                    run.result
                        .final_answer
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
                (
                    "wait_reason".to_string(),
                    run.result
                        .wait_reason
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
                (
                    "error".to_string(),
                    run.result
                        .error
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
            ]),
        );
        Ok(run)
    }
}
