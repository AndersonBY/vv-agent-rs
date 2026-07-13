//! Embedded, stateful sessions built on the public runner primitives.

mod runtime_support;

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;
use thiserror::Error;
use tokio::sync::broadcast;

use crate::agent::Agent;
use crate::approval::{ApprovalBroker, ApprovalError};
use crate::events::RunEvent;
use crate::result::RunResult;
use crate::run_config::RunConfig;
use crate::run_handle::{RunHandle, RunHandleController};
use crate::runner::Runner;
use crate::runtime::{
    background_session_manager, AfterToolCallEvent, BackgroundSessionListener,
    BackgroundSessionSubscription, BeforeLlmEvent, BeforeLlmPatch, BeforeToolCallEvent,
    BeforeToolCallPatch, CancellationToken, RuntimeHook,
};
use crate::sessions::{MemorySession, Session, SessionItem};
use crate::tools::ApprovalDecision;
use crate::types::{AgentStatus, Message, Metadata, ToolExecutionResult, ToolResultStatus};

use runtime_support::{
    build_background_command_notification, lock_queue, lock_unpoisoned, normalized_prompt,
    resolve_session, select_session_source, InteractiveRunHandleController, RunEventForwarder,
    RunLifecycleGuard, SteeringRuntimeHook,
};

type SteeringQueue = Arc<Mutex<VecDeque<String>>>;

#[derive(Debug, Error)]
pub enum InteractiveSessionError {
    #[error("{operation} prompt cannot be empty")]
    EmptyPrompt { operation: &'static str },
    #[error("interactive session `{session_id}` is already running; use steer() or follow_up()")]
    AlreadyRunning { session_id: String },
    #[error("interactive session `{session_id}` is closed")]
    Closed { session_id: String },
    #[error("interactive session `{session_id}` has no queued prompt")]
    NoQueuedPrompt { session_id: String },
    #[error("session id cannot be empty")]
    EmptySessionId,
    #[error("requested session id `{requested}` does not match storage session id `{actual}`")]
    SessionIdMismatch { requested: String, actual: String },
    #[error("session storage failed: {0}")]
    Session(String),
    #[error("runner failed for interactive session `{session_id}`: {error}")]
    Run { session_id: String, error: String },
    #[error("session query failed with status {status:?}: {reason}")]
    QueryFailed { status: AgentStatus, reason: String },
    #[error("interactive session event subscriber lagged by {missed} event(s)")]
    EventGap { missed: u64 },
    #[error("interactive session event stream is closed")]
    EventStreamClosed,
    #[error("no interactive session event available")]
    EventStreamEmpty,
}

#[derive(Clone)]
pub struct InteractiveSessionOptions {
    pub session_id: Option<String>,
    pub session: Option<Arc<dyn Session>>,
    pub run_config: RunConfig,
    pub shared_state: Metadata,
    pub event_buffer_capacity: usize,
}

impl InteractiveSessionOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn session(mut self, session: impl Session + 'static) -> Self {
        self.session = Some(Arc::new(session));
        self
    }

    pub fn session_arc(mut self, session: Arc<dyn Session>) -> Self {
        self.session = Some(session);
        self
    }

    pub fn run_config(mut self, run_config: RunConfig) -> Self {
        self.run_config = run_config;
        self
    }

    pub fn shared_state(mut self, shared_state: Metadata) -> Self {
        self.shared_state = shared_state;
        self
    }

    pub fn event_buffer_capacity(mut self, capacity: usize) -> Self {
        self.event_buffer_capacity = capacity.max(1);
        self
    }
}

impl Default for InteractiveSessionOptions {
    fn default() -> Self {
        Self {
            session_id: None,
            session: None,
            run_config: RunConfig::default(),
            shared_state: Metadata::new(),
            event_buffer_capacity: 256,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum InteractiveSessionEvent {
    RunStarted {
        session_id: String,
        prompt: String,
        existing_messages: usize,
    },
    ActiveHandleChanged {
        session_id: String,
        active: bool,
    },
    RunEvent {
        session_id: String,
        event: Box<RunEvent>,
    },
    RunEventStreamError {
        session_id: String,
        error: String,
    },
    SteerQueued {
        session_id: String,
        prompt: String,
    },
    SteerDequeued {
        session_id: String,
        prompt: String,
        cycle_index: Option<u32>,
    },
    FollowUpQueued {
        session_id: String,
        prompt: String,
    },
    FollowUpDequeued {
        session_id: String,
        prompt: String,
    },
    QueuesCleared {
        session_id: String,
    },
    CancelRequested {
        session_id: String,
    },
    MessagesReplaced {
        session_id: String,
        message_count: usize,
    },
    SharedStateReplaced {
        session_id: String,
    },
    RunFinished {
        session_id: String,
        run_id: String,
        status: AgentStatus,
        final_output: Option<String>,
    },
    RunFailed {
        session_id: String,
        error: String,
    },
    RunAborted {
        session_id: String,
    },
    SessionClosed {
        session_id: String,
        aborted: bool,
    },
    BackgroundCommandTerminal {
        session_id: String,
        background_session_id: String,
        status: String,
        notification_message: String,
        queued_to_session: bool,
        queued_to_running_session: bool,
    },
}

pub struct InteractiveSessionSubscription {
    receiver: broadcast::Receiver<InteractiveSessionEvent>,
    closed: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct InteractiveSessionState {
    pub session_id: String,
    pub running: bool,
    pub closed: bool,
    pub messages: Vec<Message>,
    pub shared_state: Metadata,
    pub latest_run: Option<RunResult>,
    pub active_handle: Option<RunHandle>,
    pub pending_steering: usize,
    pub pending_follow_ups: usize,
}

#[derive(Clone)]
pub struct InteractiveAgentClient {
    runner: Runner,
}

impl InteractiveAgentClient {
    pub fn new(runner: Runner) -> Self {
        Self { runner }
    }

    pub fn runner(&self) -> &Runner {
        &self.runner
    }

    pub async fn create_session(
        &self,
        agent: Agent,
        options: InteractiveSessionOptions,
    ) -> Result<InteractiveSession, InteractiveSessionError> {
        InteractiveSession::create(self.runner.clone(), agent, options).await
    }
}

#[derive(Clone)]
pub struct InteractiveSession {
    inner: Arc<InteractiveSessionInner>,
}

struct InteractiveSessionInner {
    runner: Runner,
    agent: Agent,
    session_id: String,
    session: Arc<dyn Session>,
    approval_broker: ApprovalBroker,
    run_config: RunConfig,
    steering: SteeringQueue,
    state: Mutex<InteractiveSessionData>,
    operation_gate: tokio::sync::Mutex<()>,
    events: broadcast::Sender<InteractiveSessionEvent>,
    closed: Arc<AtomicBool>,
    background_commands: Mutex<BTreeMap<String, Option<BackgroundSessionSubscription>>>,
}

struct InteractiveSessionData {
    running: bool,
    closed: bool,
    messages: Vec<Message>,
    shared_state: Metadata,
    latest_run: Option<RunResult>,
    active_cancellation_token: Option<CancellationToken>,
    active_handle: Option<RunHandle>,
    active_handle_controller: Option<u64>,
    follow_ups: VecDeque<String>,
}

impl InteractiveSession {
    async fn create(
        runner: Runner,
        agent: Agent,
        options: InteractiveSessionOptions,
    ) -> Result<Self, InteractiveSessionError> {
        let InteractiveSessionOptions {
            session_id: requested_session_id,
            session: option_session,
            mut run_config,
            mut shared_state,
            event_buffer_capacity,
        } = options;
        let configured_session = select_session_source(option_session, run_config.session.clone())?;
        let (session_id, session) = resolve_session(requested_session_id, configured_session)?;
        run_config.session = Some(session.clone());
        let approval_broker = run_config.approval_broker.clone().unwrap_or_default();
        run_config.approval_broker = Some(approval_broker.clone());
        let messages = session
            .get_items(None)
            .await
            .map_err(InteractiveSessionError::Session)?
            .into_iter()
            .map(|item| item.to_message())
            .collect();
        shared_state
            .entry("todo_list".to_string())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        let (events, _) = broadcast::channel(event_buffer_capacity.max(1));
        let closed = Arc::new(AtomicBool::new(false));

        Ok(Self {
            inner: Arc::new(InteractiveSessionInner {
                runner,
                agent,
                session_id,
                session,
                approval_broker,
                run_config,
                steering: SteeringQueue::default(),
                state: Mutex::new(InteractiveSessionData {
                    running: false,
                    closed: false,
                    messages,
                    shared_state,
                    latest_run: None,
                    active_cancellation_token: None,
                    active_handle: None,
                    active_handle_controller: None,
                    follow_ups: VecDeque::new(),
                }),
                operation_gate: tokio::sync::Mutex::new(()),
                events,
                closed,
                background_commands: Mutex::new(BTreeMap::new()),
            }),
        })
    }

    pub fn session_id(&self) -> &str {
        &self.inner.session_id
    }

    pub fn agent_name(&self) -> &str {
        self.inner.agent.name()
    }

    pub fn session(&self) -> Arc<dyn Session> {
        self.inner.session.clone()
    }

    pub fn subscribe(&self) -> InteractiveSessionSubscription {
        InteractiveSessionSubscription::new(
            self.inner.events.subscribe(),
            self.inner.closed.clone(),
        )
    }

    pub fn messages(&self) -> Vec<Message> {
        self.lock_state().messages.clone()
    }

    pub fn shared_state(&self) -> Metadata {
        self.lock_state().shared_state.clone()
    }

    pub fn latest_run(&self) -> Option<RunResult> {
        self.lock_state().latest_run.clone()
    }

    pub fn running(&self) -> bool {
        self.lock_state().running
    }

    pub fn closed(&self) -> bool {
        self.lock_state().closed
    }

    pub fn active_run_handle(&self) -> Option<RunHandle> {
        self.lock_state().active_handle.clone()
    }

    pub fn state(&self) -> InteractiveSessionState {
        let state = self.lock_state();
        InteractiveSessionState {
            session_id: self.inner.session_id.clone(),
            running: state.running,
            closed: state.closed,
            messages: state.messages.clone(),
            shared_state: state.shared_state.clone(),
            latest_run: state.latest_run.clone(),
            active_handle: state.active_handle.clone(),
            pending_steering: lock_queue(&self.inner.steering).len(),
            pending_follow_ups: state.follow_ups.len(),
        }
    }

    pub fn close(&self) -> bool {
        let (was_running, token, handle, controller) = {
            let mut state = self.lock_state();
            if state.closed {
                return false;
            }
            state.closed = true;
            let was_running = state.running;
            let token = state.active_cancellation_token.take();
            let handle = state.active_handle.take();
            let controller = state.active_handle_controller.take();
            state.follow_ups.clear();
            (was_running, token, handle, controller)
        };

        lock_queue(&self.inner.steering).clear();
        lock_unpoisoned(&self.inner.background_commands).clear();
        if let Some(token) = token {
            token.cancel_with_reason("interactive session closed");
        }
        if let Some(handle) = handle {
            if let Some(controller) = controller {
                handle.detach_controller(controller);
            }
            handle.cancel_with_reason("interactive session closed");
            self.emit(InteractiveSessionEvent::ActiveHandleChanged {
                session_id: self.inner.session_id.clone(),
                active: false,
            });
        }
        let _ = self
            .inner
            .approval_broker
            .cancel_pending("interactive session closed");
        self.emit(InteractiveSessionEvent::SessionClosed {
            session_id: self.inner.session_id.clone(),
            aborted: was_running,
        });
        self.inner.closed.store(true, Ordering::SeqCst);
        true
    }

    pub fn steer(&self, prompt: impl Into<String>) -> Result<(), InteractiveSessionError> {
        let prompt = normalized_prompt(prompt, "steer")?;
        self.ensure_open()?;
        lock_queue(&self.inner.steering).push_back(prompt.clone());
        self.emit(InteractiveSessionEvent::SteerQueued {
            session_id: self.inner.session_id.clone(),
            prompt,
        });
        Ok(())
    }

    pub fn follow_up(&self, prompt: impl Into<String>) -> Result<(), InteractiveSessionError> {
        let prompt = normalized_prompt(prompt, "follow_up")?;
        self.ensure_open()?;
        self.lock_state().follow_ups.push_back(prompt.clone());
        self.emit(InteractiveSessionEvent::FollowUpQueued {
            session_id: self.inner.session_id.clone(),
            prompt,
        });
        Ok(())
    }

    pub fn clear_queues(&self) -> Result<(), InteractiveSessionError> {
        self.ensure_open()?;
        lock_queue(&self.inner.steering).clear();
        self.lock_state().follow_ups.clear();
        self.emit(InteractiveSessionEvent::QueuesCleared {
            session_id: self.inner.session_id.clone(),
        });
        Ok(())
    }

    pub fn approve(
        &self,
        request_id: impl AsRef<str>,
        decision: ApprovalDecision,
    ) -> Result<(), ApprovalError> {
        if self.closed() {
            return Err(ApprovalError::new(format!(
                "interactive session `{}` is closed",
                self.inner.session_id
            )));
        }
        self.inner.approval_broker.resolve(request_id, decision)
    }

    pub fn cancel(&self) -> bool {
        let (token, handle) = {
            let mut state = self.lock_state();
            let Some(token) = state.active_cancellation_token.clone() else {
                return false;
            };
            if !state.running || token.is_cancelled() {
                return false;
            }
            state.follow_ups.clear();
            (token, state.active_handle.clone())
        };
        lock_queue(&self.inner.steering).clear();
        if let Some(handle) = handle {
            handle.cancel();
        }
        token.cancel();
        self.emit(InteractiveSessionEvent::CancelRequested {
            session_id: self.inner.session_id.clone(),
        });
        true
    }

    pub async fn prompt(
        &self,
        prompt: impl Into<String>,
    ) -> Result<RunResult, InteractiveSessionError> {
        let prompt = normalized_prompt(prompt, "prompt")?;
        self.ensure_open()?;
        let _operation = self.try_operation()?;
        self.run_prompt_chain(prompt, true).await
    }

    pub async fn prompt_once(
        &self,
        prompt: impl Into<String>,
    ) -> Result<RunResult, InteractiveSessionError> {
        let prompt = normalized_prompt(prompt, "prompt")?;
        self.ensure_open()?;
        let _operation = self.try_operation()?;
        self.run_prompt_chain(prompt, false).await
    }

    pub async fn continue_run(
        &self,
        prompt: Option<&str>,
    ) -> Result<RunResult, InteractiveSessionError> {
        self.ensure_open()?;
        let _operation = self.try_operation()?;
        let prompt = match prompt.map(str::trim).filter(|value| !value.is_empty()) {
            Some(prompt) => prompt.to_string(),
            None => self.pop_queued_prompt()?,
        };
        self.run_prompt_chain(prompt, false).await
    }

    pub async fn query(
        &self,
        prompt: impl Into<String>,
    ) -> Result<String, InteractiveSessionError> {
        let result = self.prompt(prompt).await?;
        if result.status() == AgentStatus::Completed {
            return Ok(result.final_output().unwrap_or_default().to_string());
        }
        Err(InteractiveSessionError::QueryFailed {
            status: result.status(),
            reason: result
                .result()
                .error
                .clone()
                .or_else(|| result.result().wait_reason.clone())
                .or_else(|| result.final_output().map(str::to_string))
                .unwrap_or_else(|| "session query did not complete".to_string()),
        })
    }

    pub async fn replace_messages(
        &self,
        messages: Vec<Message>,
    ) -> Result<(), InteractiveSessionError> {
        self.ensure_open()?;
        let _operation = self.try_operation()?;
        self.ensure_not_running()?;
        let items = messages
            .iter()
            .filter_map(SessionItem::from_message)
            .collect::<Vec<_>>();
        self.inner
            .session
            .clear()
            .await
            .map_err(InteractiveSessionError::Session)?;
        self.inner
            .session
            .add_items(items)
            .await
            .map_err(InteractiveSessionError::Session)?;
        self.lock_state().messages = messages.clone();
        self.emit(InteractiveSessionEvent::MessagesReplaced {
            session_id: self.inner.session_id.clone(),
            message_count: messages.len(),
        });
        Ok(())
    }

    pub async fn replace_shared_state(
        &self,
        mut shared_state: Metadata,
    ) -> Result<(), InteractiveSessionError> {
        self.ensure_open()?;
        let _operation = self.try_operation()?;
        self.ensure_not_running()?;
        shared_state
            .entry("todo_list".to_string())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        self.lock_state().shared_state = shared_state;
        self.emit(InteractiveSessionEvent::SharedStateReplaced {
            session_id: self.inner.session_id.clone(),
        });
        Ok(())
    }

    async fn run_prompt_chain(
        &self,
        first_prompt: String,
        auto_follow_up: bool,
    ) -> Result<RunResult, InteractiveSessionError> {
        let mut lifecycle = RunLifecycleGuard::begin(self.inner.clone())?;
        let mut prompt = first_prompt;
        let outcome = loop {
            match self.execute_once(prompt).await {
                Ok(result) => {
                    if auto_follow_up && result.status() == AgentStatus::Completed {
                        if let Some(follow_up) = self.pop_follow_up() {
                            prompt = follow_up;
                            continue;
                        }
                    }
                    break Ok(result);
                }
                Err(error) => break Err(error),
            }
        };
        lifecycle.finish();
        outcome
    }

    async fn execute_once(&self, prompt: String) -> Result<RunResult, InteractiveSessionError> {
        let existing_messages = self.lock_state().messages.len();
        self.emit(InteractiveSessionEvent::RunStarted {
            session_id: self.inner.session_id.clone(),
            prompt: prompt.clone(),
            existing_messages,
        });

        let mut config = self.inner.run_config.clone();
        self.inner
            .approval_broker
            .reset_cancelled()
            .map_err(|error| self.run_error(error.to_string()))?;
        config.session = Some(self.inner.session.clone());
        config.cancellation_token = self.lock_state().active_cancellation_token.clone();
        config.approval_broker = Some(self.inner.approval_broker.clone());
        config
            .initial_shared_state
            .extend(self.lock_state().shared_state.clone());
        config.metadata.insert(
            "session_id".to_string(),
            serde_json::Value::String(self.inner.session_id.clone()),
        );
        config.hooks.push(Arc::new(SteeringRuntimeHook {
            queue: self.inner.steering.clone(),
            session_id: self.inner.session_id.clone(),
            events: self.inner.events.clone(),
            inner: Arc::downgrade(&self.inner),
        }));

        let handle = match self
            .inner
            .runner
            .start(&self.inner.agent, prompt, config)
            .await
        {
            Ok(handle) => handle,
            Err(error) => return Err(self.run_error(error)),
        };
        self.set_active_handle(Some(handle.clone()));
        let mut event_forwarder = self.forward_run_events(handle.events());
        let result = handle.result().await;
        self.set_active_handle(None);
        match event_forwarder.finish().await {
            Ok(()) => {}
            Err(error) => self.emit(InteractiveSessionEvent::RunEventStreamError {
                session_id: self.inner.session_id.clone(),
                error: format!("run event forwarding task failed: {error}"),
            }),
        }

        if self.closed() {
            return Err(InteractiveSessionError::Closed {
                session_id: self.inner.session_id.clone(),
            });
        }

        let result = match result {
            Ok(result) => result,
            Err(error) => return Err(self.run_error(error)),
        };
        self.sync_background_command_watchers(&result);
        let messages = self
            .inner
            .session
            .get_items(None)
            .await
            .map_err(InteractiveSessionError::Session)?
            .into_iter()
            .map(|item| item.to_message())
            .collect::<Vec<_>>();
        {
            let mut state = self.lock_state();
            state.messages = messages;
            state.shared_state = result.result().shared_state.clone();
            state.latest_run = Some(result.clone());
        }
        self.emit(InteractiveSessionEvent::RunFinished {
            session_id: self.inner.session_id.clone(),
            run_id: result.run_id().to_string(),
            status: result.status(),
            final_output: result.final_output().map(str::to_string),
        });
        Ok(result)
    }

    fn sync_background_command_watchers(&self, result: &RunResult) {
        for cycle in &result.result().cycles {
            for (index, tool_result) in cycle.tool_results.iter().enumerate() {
                let tool_name = cycle
                    .tool_calls
                    .iter()
                    .find(|call| call.id == tool_result.tool_call_id)
                    .or_else(|| cycle.tool_calls.get(index))
                    .map(|call| call.name.trim().to_ascii_lowercase());
                if let Some(tool_name) = tool_name {
                    self.sync_background_command_result(&tool_name, tool_result);
                }
            }
        }
    }

    fn sync_background_command_result(&self, tool_name: &str, tool_result: &ToolExecutionResult) {
        if !matches!(
            tool_name.trim().to_ascii_lowercase().as_str(),
            "bash" | "check_background_command"
        ) {
            return;
        }
        let Some(background_session_id) = tool_result
            .metadata
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return;
        };
        let status = tool_result
            .metadata
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if status == "running" || tool_result.status == ToolResultStatus::Running {
            self.subscribe_background_command(background_session_id);
        } else if matches!(
            status.as_str(),
            "completed" | "failed" | "timeout" | "missing"
        ) {
            self.unsubscribe_background_command(background_session_id);
        }
    }

    fn subscribe_background_command(&self, background_session_id: &str) {
        let background_session_id = background_session_id.trim();
        if background_session_id.is_empty() {
            return;
        }
        {
            let mut subscriptions = lock_unpoisoned(&self.inner.background_commands);
            if subscriptions.contains_key(background_session_id) {
                return;
            }
            subscriptions.insert(background_session_id.to_string(), None);
        }

        let weak_inner = Arc::downgrade(&self.inner);
        let callback_session_id = background_session_id.to_string();
        let listener: BackgroundSessionListener = Arc::new(move |payload| {
            if let Some(inner) = weak_inner.upgrade() {
                InteractiveSession::handle_background_command_terminal(
                    &inner,
                    &callback_session_id,
                    payload,
                );
            }
        });
        let subscription = background_session_manager().subscribe(background_session_id, listener);
        let mut subscriptions = lock_unpoisoned(&self.inner.background_commands);
        if let Some(slot) = subscriptions.get_mut(background_session_id) {
            *slot = Some(subscription);
        }
    }

    fn unsubscribe_background_command(&self, background_session_id: &str) {
        lock_unpoisoned(&self.inner.background_commands).remove(background_session_id.trim());
    }

    fn handle_background_command_terminal(
        inner: &Arc<InteractiveSessionInner>,
        background_session_id: &str,
        payload: &Value,
    ) {
        if inner.closed.load(Ordering::SeqCst) {
            return;
        }
        lock_unpoisoned(&inner.background_commands).remove(background_session_id);
        let notification_message = build_background_command_notification(payload);
        let state = lock_unpoisoned(&inner.state);
        let running = state.running && !state.closed;
        drop(state);
        if running {
            lock_queue(&inner.steering).push_back(notification_message.clone());
            let _ = inner.events.send(InteractiveSessionEvent::SteerQueued {
                session_id: inner.session_id.clone(),
                prompt: notification_message.clone(),
            });
        }
        let _ = inner
            .events
            .send(InteractiveSessionEvent::BackgroundCommandTerminal {
                session_id: inner.session_id.clone(),
                background_session_id: background_session_id.to_string(),
                status: payload
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("terminal")
                    .trim()
                    .to_ascii_lowercase(),
                notification_message,
                queued_to_session: running,
                queued_to_running_session: running,
            });
    }

    fn forward_run_events(&self, mut stream: crate::runner::RunEventStream) -> RunEventForwarder {
        let events = self.inner.events.clone();
        let session_id = self.inner.session_id.clone();
        let closed = self.inner.closed.clone();
        RunEventForwarder::new(tokio::spawn(async move {
            while let Some(event) = stream.next().await {
                if closed.load(Ordering::SeqCst) {
                    break;
                }
                match event {
                    Ok(event) => {
                        let event = if event.session_id().is_some() {
                            event
                        } else {
                            event.with_session_id(session_id.clone())
                        };
                        let _ = events.send(InteractiveSessionEvent::RunEvent {
                            session_id: session_id.clone(),
                            event: Box::new(event),
                        });
                    }
                    Err(error) => {
                        let _ = events.send(InteractiveSessionEvent::RunEventStreamError {
                            session_id: session_id.clone(),
                            error,
                        });
                    }
                }
            }
        }))
    }

    fn pop_follow_up(&self) -> Option<String> {
        let prompt = self.lock_state().follow_ups.pop_front()?;
        self.emit(InteractiveSessionEvent::FollowUpDequeued {
            session_id: self.inner.session_id.clone(),
            prompt: prompt.clone(),
        });
        Some(prompt)
    }

    fn pop_queued_prompt(&self) -> Result<String, InteractiveSessionError> {
        if let Some(prompt) = lock_queue(&self.inner.steering).pop_front() {
            self.emit(InteractiveSessionEvent::SteerDequeued {
                session_id: self.inner.session_id.clone(),
                prompt: prompt.clone(),
                cycle_index: None,
            });
            return Ok(prompt);
        }
        self.pop_follow_up()
            .ok_or_else(|| InteractiveSessionError::NoQueuedPrompt {
                session_id: self.inner.session_id.clone(),
            })
    }

    fn ensure_not_running(&self) -> Result<(), InteractiveSessionError> {
        self.ensure_open()?;
        if self.running() {
            return Err(InteractiveSessionError::AlreadyRunning {
                session_id: self.inner.session_id.clone(),
            });
        }
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), InteractiveSessionError> {
        if self.closed() {
            return Err(InteractiveSessionError::Closed {
                session_id: self.inner.session_id.clone(),
            });
        }
        Ok(())
    }

    fn try_operation(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, InteractiveSessionError> {
        self.inner
            .operation_gate
            .try_lock()
            .map_err(|_| InteractiveSessionError::AlreadyRunning {
                session_id: self.inner.session_id.clone(),
            })
    }

    fn set_active_handle(&self, handle: Option<RunHandle>) {
        let controller = handle.as_ref().map(|handle| {
            handle.attach_controller(Arc::new(InteractiveRunHandleController {
                inner: Arc::downgrade(&self.inner),
                session_id: self.inner.session_id.clone(),
            }))
        });
        let (previous_handle, previous_controller, rejected, changed) = {
            let mut state = self.lock_state();
            if state.closed && handle.is_some() {
                (None, None, true, false)
            } else {
                let changed = state.active_handle.is_some() != handle.is_some();
                let previous_handle = std::mem::replace(&mut state.active_handle, handle.clone());
                let previous_controller =
                    std::mem::replace(&mut state.active_handle_controller, controller);
                (previous_handle, previous_controller, false, changed)
            }
        };
        if rejected {
            if let (Some(handle), Some(controller)) = (handle, controller) {
                handle.detach_controller(controller);
                handle.cancel_with_reason("interactive session closed");
            }
            return;
        }
        if let (Some(previous_handle), Some(previous_controller)) =
            (previous_handle, previous_controller)
        {
            previous_handle.detach_controller(previous_controller);
        }
        if changed {
            self.emit(InteractiveSessionEvent::ActiveHandleChanged {
                session_id: self.inner.session_id.clone(),
                active: handle.is_some(),
            });
        }
    }

    fn run_error(&self, error: String) -> InteractiveSessionError {
        self.emit(InteractiveSessionEvent::RunFailed {
            session_id: self.inner.session_id.clone(),
            error: error.clone(),
        });
        InteractiveSessionError::Run {
            session_id: self.inner.session_id.clone(),
            error,
        }
    }

    fn emit(&self, event: InteractiveSessionEvent) {
        if self.inner.closed.load(Ordering::SeqCst)
            && !matches!(&event, InteractiveSessionEvent::SessionClosed { .. })
        {
            return;
        }
        let _ = self.inner.events.send(event);
    }

    fn lock_state(&self) -> MutexGuard<'_, InteractiveSessionData> {
        lock_unpoisoned(&self.inner.state)
    }
}

pub async fn create_interactive_session(
    runner: &Runner,
    agent: Agent,
    options: InteractiveSessionOptions,
) -> Result<InteractiveSession, InteractiveSessionError> {
    InteractiveSession::create(runner.clone(), agent, options).await
}
