use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::llm::LlmClient;
use crate::runtime::sub_agent_sessions::{
    SubAgentSession, SubAgentSessionListener, SubAgentSessionUnsubscribe,
};
use crate::runtime::{AgentRuntime, ExecutionContext, RuntimeRunControls, StreamCallback};
use crate::types::{AgentResult, SubTaskOutcome};

use super::events::{
    agent_status_value, emit_parent_sub_agent_event, emit_sub_agent_session_event,
    enrich_sub_agent_payload,
};
use super::types::{RuntimeSubAgentSessionParts, RuntimeSubAgentSessionState};

pub(super) struct RuntimeSubAgentSession {
    llm_client: Arc<dyn LlmClient>,
    tool_registry: crate::tools::ToolRegistry,
    workspace_path: std::path::PathBuf,
    workspace_backend: Arc<dyn crate::workspace::WorkspaceBackend>,
    pub(super) task_template: crate::types::AgentTask,
    task_id: String,
    agent_name: String,
    session_id: String,
    resolved: BTreeMap<String, String>,
    stream_callback: Option<StreamCallback>,
    parent_log_handler: Option<crate::runtime::RuntimeLogHandler>,
    parent_event_handler: Option<crate::runtime::RuntimeEventHandler>,
    state: Mutex<RuntimeSubAgentSessionState>,
    running: Mutex<bool>,
    steering_queue: Arc<Mutex<VecDeque<String>>>,
    listeners: Arc<Mutex<BTreeMap<u64, SubAgentSessionListener>>>,
    next_listener_id: AtomicU64,
}

impl RuntimeSubAgentSession {
    pub(super) fn new(parts: RuntimeSubAgentSessionParts) -> Self {
        let task_id = parts.task_template.task_id.clone();
        Self {
            llm_client: parts.llm_client,
            tool_registry: parts.tool_registry,
            workspace_path: parts.workspace_path,
            workspace_backend: parts.workspace_backend,
            task_template: parts.task_template,
            task_id,
            agent_name: parts.agent_name,
            session_id: parts.session_id,
            resolved: parts.resolved,
            stream_callback: parts.stream_callback,
            parent_log_handler: parts.parent_log_handler,
            parent_event_handler: parts.parent_event_handler,
            state: Mutex::new(RuntimeSubAgentSessionState::default()),
            running: Mutex::new(false),
            steering_queue: Arc::new(Mutex::new(VecDeque::new())),
            listeners: Arc::new(Mutex::new(BTreeMap::new())),
            next_listener_id: AtomicU64::new(1),
        }
    }

    fn run_prompt(&self, prompt: &str) -> Result<SubTaskOutcome, String> {
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

    fn emit_session_run_end(&self, result: &AgentResult) {
        self.emit(
            "session_run_end",
            BTreeMap::from([
                (
                    "status".to_string(),
                    Value::String(agent_status_value(result.status).to_string()),
                ),
                (
                    "cycles".to_string(),
                    Value::from(result.cycles.len() as u64),
                ),
                (
                    "final_answer".to_string(),
                    result
                        .final_answer
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
                (
                    "wait_reason".to_string(),
                    result
                        .wait_reason
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
                (
                    "error".to_string(),
                    result
                        .error
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
            ]),
        );
    }

    pub(super) fn emit(&self, event: &str, payload: BTreeMap<String, Value>) {
        emit_sub_agent_session_event(&self.listeners, event, &payload);
        let enriched =
            enrich_sub_agent_payload(&payload, &self.task_id, &self.session_id, &self.agent_name);
        emit_parent_sub_agent_event(
            &self.parent_log_handler,
            &self.parent_event_handler,
            &format!("sub_agent_{event}"),
            enriched,
        );
    }
}

impl SubAgentSession for RuntimeSubAgentSession {
    fn steer(&self, prompt: &str) -> Result<(), String> {
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

    fn sanitize_for_resume(&self) -> usize {
        let Ok(mut state) = self.state.lock() else {
            return 0;
        };
        let sanitized = crate::memory::sanitize_for_resume(&state.messages);
        let removed = state.messages.len().saturating_sub(sanitized.len());
        state.messages = sanitized;
        removed
    }

    fn continue_run(&self, prompt: &str) -> Result<SubTaskOutcome, String> {
        self.run_prompt(prompt)
    }

    fn subscribe(&self, listener: SubAgentSessionListener) -> Option<SubAgentSessionUnsubscribe> {
        let listener_id = self.next_listener_id.fetch_add(1, Ordering::Relaxed);
        self.listeners
            .lock()
            .expect("sub-agent session listeners poisoned")
            .insert(listener_id, listener);
        let listeners = self.listeners.clone();
        Some(Box::new(move || {
            if let Ok(mut listeners) = listeners.lock() {
                listeners.remove(&listener_id);
            }
        }))
    }
}
