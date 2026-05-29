use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::runtime::sub_agents::events::{
    emit_parent_sub_agent_event, emit_sub_agent_session_event, enrich_sub_agent_payload,
};
use crate::runtime::{AgentRuntime, ExecutionContext, RuntimeRunControls, StreamCallback};
use crate::types::{AgentResult, SubTaskOutcome};

use super::RuntimeSubAgentSession;

impl RuntimeSubAgentSession {
    pub(super) fn run_prompt(&self, prompt: &str) -> Result<SubTaskOutcome, String> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err("Follow-up prompt cannot be empty.".to_string());
        }
        {
            let mut running = self
                .running
                .lock()
                .map_err(|_| "Sub-agent session running lock is poisoned.".to_string())?;
            if *running {
                return Err("Sub-agent session is already running.".to_string());
            }
            *running = true;
        }
        let result = self.run_prompt_inner(prompt);
        if let Ok(mut running) = self.running.lock() {
            *running = false;
        }
        result
    }

    fn run_prompt_inner(&self, prompt: &str) -> Result<SubTaskOutcome, String> {
        let (initial_messages, shared_state) = {
            let state = self
                .state
                .lock()
                .map_err(|_| "Sub-agent session state lock is poisoned.".to_string())?;
            (state.messages.clone(), state.shared_state.clone())
        };
        self.emit(
            "session_run_start",
            BTreeMap::from([
                ("prompt".to_string(), Value::String(prompt.to_string())),
                (
                    "existing_messages".to_string(),
                    Value::from(initial_messages.len() as u64),
                ),
            ]),
        );

        let mut task = self.task_template.clone();
        task.user_prompt = prompt.to_string();
        task.initial_messages = initial_messages;
        task.initial_shared_state = shared_state;

        let listeners = self.listeners.clone();
        let parent_log_handler = self.parent_log_handler.clone();
        let parent_event_handler = self.parent_event_handler.clone();
        let task_id = self.task_id.clone();
        let session_id = self.session_id.clone();
        let agent_name = self.agent_name.clone();
        let log_handler = Arc::new(move |event: &str, payload: &BTreeMap<String, Value>| {
            emit_sub_agent_session_event(&listeners, event, payload);
            let enriched = enrich_sub_agent_payload(payload, &task_id, &session_id, &agent_name);
            emit_parent_sub_agent_event(
                &parent_log_handler,
                &parent_event_handler,
                &format!("sub_agent_{event}"),
                enriched,
            );
        });
        let mut runtime = AgentRuntime::new(self.llm_client.clone())
            .with_tool_registry(self.tool_registry.clone());
        runtime.default_workspace = Some(self.workspace_path.clone());
        runtime.workspace_backend = self.workspace_backend.clone();
        let execution_context = self.stream_callback.clone().map(|callback| {
            let parent_log_handler = self.parent_log_handler.clone();
            let parent_event_handler = self.parent_event_handler.clone();
            let task_id = self.task_id.clone();
            let session_id = self.session_id.clone();
            let agent_name = self.agent_name.clone();
            let stream_callback: StreamCallback = Arc::new(move |event| {
                let enriched = enrich_sub_agent_payload(event, &task_id, &session_id, &agent_name);
                let event_name = enriched
                    .get("event")
                    .and_then(Value::as_str)
                    .unwrap_or("stream_event")
                    .to_string();
                let mut log_payload = enriched.clone();
                log_payload.remove("event");
                emit_parent_sub_agent_event(
                    &parent_log_handler,
                    &parent_event_handler,
                    &format!("sub_agent_{event_name}"),
                    log_payload,
                );
                callback(&enriched);
            });
            ExecutionContext::default().with_stream_callback(stream_callback)
        });
        let result = runtime
            .run_with_controls(
                task,
                RuntimeRunControls {
                    log_handler: Some(log_handler),
                    before_cycle_messages: None,
                    interruption_messages: None,
                    steering_queue: Some(self.steering_queue.clone()),
                    cancellation_token: None,
                    execution_context,
                    workspace: None,
                    workspace_backend: None,
                    sub_task_manager: None,
                },
            )
            .map_err(|error| error.to_string())?;

        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "Sub-agent session state lock is poisoned.".to_string())?;
            state.messages = result.messages.clone();
            state.shared_state = result.shared_state.clone();
        }
        self.emit_session_run_end(&result);
        Ok(self.outcome_from_result(result))
    }

    fn outcome_from_result(&self, result: AgentResult) -> SubTaskOutcome {
        let todo_list = result.todo_list();
        let cycles = result.cycles.len() as u32;
        SubTaskOutcome {
            task_id: self.task_id.clone(),
            agent_name: self.agent_name.clone(),
            status: result.status,
            session_id: Some(self.session_id.clone()),
            final_answer: result.final_answer,
            wait_reason: result.wait_reason,
            error: result.error,
            cycles,
            todo_list,
            resolved: self.resolved.clone(),
        }
    }
}
