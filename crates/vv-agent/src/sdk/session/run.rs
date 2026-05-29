use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::runtime::CancellationToken;
use crate::types::AgentStatus;

use super::events::emit_session_event;
use super::handles::{SessionCancellationHandle, SessionSteeringHandle};
use super::state::{AgentSessionRunRequest, SessionEventHandler};
use super::util::normalize_session_prompt;
use super::watchers::sync_background_command_watchers;
use super::AgentSession;
use crate::sdk::types::{agent_status_value, AgentRun};

impl AgentSession {
    pub fn prompt(&mut self, prompt: impl Into<String>) -> Result<AgentRun, String> {
        self.prompt_with_auto_follow_up(prompt, true)
    }

    pub fn prompt_with_auto_follow_up(
        &mut self,
        prompt: impl Into<String>,
        auto_follow_up: bool,
    ) -> Result<AgentRun, String> {
        let mut run = self.run_once(normalize_session_prompt(prompt.into(), "prompt")?)?;
        if !auto_follow_up {
            return Ok(run);
        }

        while run.result.status == AgentStatus::Completed {
            let follow_up_prompt = self
                .follow_up_queue
                .lock()
                .expect("session follow-up queue lock")
                .pop_front();
            let Some(follow_up_prompt) = follow_up_prompt else {
                break;
            };
            self.emit(
                "session_follow_up_dequeued",
                BTreeMap::from([(
                    "prompt".to_string(),
                    Value::String(follow_up_prompt.clone()),
                )]),
            );
            run = self.run_once(follow_up_prompt)?;
        }
        Ok(run)
    }

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

    pub fn query(&mut self, prompt: impl Into<String>) -> Result<String, String> {
        self.query_with_require_completed(prompt, true)
    }

    pub fn query_with_require_completed(
        &mut self,
        prompt: impl Into<String>,
        require_completed: bool,
    ) -> Result<String, String> {
        let run = self.prompt(prompt)?;
        if run.result.status == AgentStatus::Completed {
            return Ok(run.result.final_answer.unwrap_or_default());
        }
        if require_completed {
            let reason = run
                .result
                .error
                .clone()
                .or(run.result.wait_reason.clone())
                .or(run.result.final_answer.clone())
                .unwrap_or_else(|| "session query did not complete".to_string());
            return Err(format!(
                "Session query failed with status={}: {}",
                agent_status_value(run.result.status),
                reason
            ));
        }
        Ok(run
            .result
            .final_answer
            .or(run.result.wait_reason)
            .or(run.result.error)
            .unwrap_or_default())
    }

    fn run_once(&mut self, prompt: String) -> Result<AgentRun, String> {
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
