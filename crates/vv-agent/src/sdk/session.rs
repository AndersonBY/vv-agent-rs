use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::background_sessions::{background_session_manager, BackgroundSessionSubscription};
use crate::runtime::{
    BeforeCycleMessageProvider, CancellationToken, InterruptionMessageProvider, StreamCallback,
};
use crate::sub_task_manager::SubTaskManager;
use crate::types::{AgentStatus, Metadata};

use super::types::{agent_status_value, AgentDefinition, AgentRun};

static SESSION_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct AgentSessionState {
    pub running: bool,
    pub session_id: String,
    pub workspace: PathBuf,
    pub messages: Vec<crate::types::Message>,
    pub shared_state: Metadata,
    pub latest_run: Option<AgentRun>,
}

pub type SessionEventHandler = Arc<dyn Fn(&str, &BTreeMap<String, Value>) + Send + Sync + 'static>;
pub type SessionListenerId = u64;

#[derive(Clone, Default)]
pub struct AgentSessionRunRequest {
    pub prompt: String,
    pub task_name: Option<String>,
    pub workspace: Option<PathBuf>,
    pub initial_messages: Vec<crate::types::Message>,
    pub shared_state: Metadata,
    pub metadata: Metadata,
    pub runtime_event_handler: Option<SessionEventHandler>,
    pub before_cycle_messages: Option<BeforeCycleMessageProvider>,
    pub interruption_messages: Option<InterruptionMessageProvider>,
    pub steering_queue: Option<Arc<Mutex<VecDeque<String>>>>,
    pub cancellation_token: Option<CancellationToken>,
    pub stream_callback: Option<StreamCallback>,
    pub sub_task_manager: Option<SubTaskManager>,
}

impl AgentSessionRunRequest {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            task_name: None,
            workspace: None,
            initial_messages: Vec::new(),
            shared_state: Metadata::new(),
            metadata: Metadata::new(),
            runtime_event_handler: None,
            before_cycle_messages: None,
            interruption_messages: None,
            steering_queue: None,
            cancellation_token: None,
            stream_callback: None,
            sub_task_manager: None,
        }
    }
}

#[derive(Clone)]
pub struct SessionSteeringHandle {
    steering_queue: Arc<Mutex<VecDeque<String>>>,
    listeners: Arc<Mutex<BTreeMap<SessionListenerId, SessionEventHandler>>>,
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
    active_cancellation_token: Arc<Mutex<Option<CancellationToken>>>,
    steering_queue: Arc<Mutex<VecDeque<String>>>,
    follow_up_queue: Arc<Mutex<VecDeque<String>>>,
    listeners: Arc<Mutex<BTreeMap<SessionListenerId, SessionEventHandler>>>,
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

pub struct AgentSession {
    execute_run: Arc<dyn Fn(AgentSessionRunRequest) -> Result<AgentRun, String> + Send + Sync>,
    session_id: String,
    _agent_name: String,
    _definition: AgentDefinition,
    workspace: PathBuf,
    shared_state: Metadata,
    messages: Vec<crate::types::Message>,
    latest_run: Option<AgentRun>,
    running: bool,
    listeners: Arc<Mutex<BTreeMap<SessionListenerId, SessionEventHandler>>>,
    background_command_subscriptions: Arc<Mutex<BTreeMap<String, BackgroundSessionSubscription>>>,
    next_listener_id: SessionListenerId,
    steering_queue: Arc<Mutex<VecDeque<String>>>,
    follow_up_queue: Arc<Mutex<VecDeque<String>>>,
    active_cancellation_token: Arc<Mutex<Option<CancellationToken>>>,
    sub_task_manager: SubTaskManager,
}

impl AgentSession {
    pub fn new(
        execute_run: Arc<dyn Fn(String) -> Result<AgentRun, String> + Send + Sync>,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        let execute_run =
            Arc::new(move |request: AgentSessionRunRequest| execute_run(request.prompt));
        Self::new_with_context(execute_run, agent_name, definition, workspace)
    }

    pub fn new_with_session_id(
        execute_run: Arc<dyn Fn(String) -> Result<AgentRun, String> + Send + Sync>,
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        let execute_run =
            Arc::new(move |request: AgentSessionRunRequest| execute_run(request.prompt));
        Self::new_with_context_and_session_id(
            execute_run,
            session_id,
            agent_name,
            definition,
            workspace,
        )
    }

    pub fn new_with_context(
        execute_run: Arc<dyn Fn(AgentSessionRunRequest) -> Result<AgentRun, String> + Send + Sync>,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        Self::new_with_context_and_session_id(
            execute_run,
            generate_session_id(),
            agent_name,
            definition,
            workspace,
        )
    }

    pub fn new_with_context_and_shared_state(
        execute_run: Arc<dyn Fn(AgentSessionRunRequest) -> Result<AgentRun, String> + Send + Sync>,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
        shared_state: Metadata,
    ) -> Self {
        Self::new_with_context_and_session_id_and_shared_state(
            execute_run,
            generate_session_id(),
            agent_name,
            definition,
            workspace,
            shared_state,
        )
    }

    pub fn new_with_context_and_session_id(
        execute_run: Arc<dyn Fn(AgentSessionRunRequest) -> Result<AgentRun, String> + Send + Sync>,
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        Self::new_with_context_and_session_id_and_shared_state(
            execute_run,
            session_id,
            agent_name,
            definition,
            workspace,
            Metadata::new(),
        )
    }

    pub fn new_with_context_and_session_id_and_shared_state(
        execute_run: Arc<dyn Fn(AgentSessionRunRequest) -> Result<AgentRun, String> + Send + Sync>,
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
        shared_state: Metadata,
    ) -> Self {
        let mut shared_state = shared_state;
        shared_state
            .entry("todo_list".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let workspace = absolutize_workspace(workspace.into());
        Self {
            execute_run,
            session_id: normalize_session_id(session_id),
            _agent_name: agent_name.into(),
            _definition: definition,
            workspace,
            shared_state,
            messages: Vec::new(),
            latest_run: None,
            running: false,
            listeners: Arc::new(Mutex::new(BTreeMap::new())),
            background_command_subscriptions: Arc::new(Mutex::new(BTreeMap::new())),
            next_listener_id: 1,
            steering_queue: Arc::new(Mutex::new(VecDeque::new())),
            follow_up_queue: Arc::new(Mutex::new(VecDeque::new())),
            active_cancellation_token: Arc::new(Mutex::new(None)),
            sub_task_manager: SubTaskManager::default(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn agent_name(&self) -> &str {
        &self._agent_name
    }

    pub fn definition(&self) -> &AgentDefinition {
        &self._definition
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn messages(&self) -> Vec<crate::types::Message> {
        self.messages.clone()
    }

    pub fn shared_state(&self) -> Metadata {
        self.shared_state.clone()
    }

    pub fn latest_run(&self) -> Option<AgentRun> {
        self.latest_run.clone()
    }

    pub fn running(&self) -> bool {
        self.running
    }

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

    fn emit(&self, event: &str, payload: BTreeMap<String, Value>) {
        emit_session_event(&self.listeners, event, payload);
    }

    pub fn state(&self) -> AgentSessionState {
        AgentSessionState {
            running: self.running,
            session_id: self.session_id.clone(),
            workspace: self.workspace.clone(),
            messages: self.messages.clone(),
            shared_state: self.shared_state.clone(),
            latest_run: self.latest_run.clone(),
        }
    }
}

fn emit_session_event(
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

fn sync_background_command_watchers(
    subscriptions: &Arc<Mutex<BTreeMap<String, BackgroundSessionSubscription>>>,
    listeners: &Arc<Mutex<BTreeMap<SessionListenerId, SessionEventHandler>>>,
    steering_handle: &SessionSteeringHandle,
    event: &str,
    payload: &BTreeMap<String, Value>,
) {
    if event != "tool_result" {
        return;
    }
    let tool_name = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if tool_name != "bash" && tool_name != "check_background_command" {
        return;
    }
    let metadata = payload.get("metadata").and_then(Value::as_object);
    let session_id = metadata
        .and_then(|metadata| metadata.get("session_id"))
        .or_else(|| payload.get("session_id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if session_id.is_empty() {
        return;
    }
    let status = metadata
        .and_then(|metadata| metadata.get("status"))
        .or_else(|| payload.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    if status == "running" {
        let mut subscriptions = subscriptions
            .lock()
            .expect("background command subscriptions lock");
        if subscriptions.contains_key(&session_id) {
            return;
        }
        let listener_session_id = session_id.clone();
        let listener_events = Arc::clone(listeners);
        let listener_steering = steering_handle.clone();
        let subscription = background_session_manager().subscribe(
            &session_id,
            Arc::new(move |payload| {
                handle_background_command_terminal(
                    &listener_events,
                    &listener_steering,
                    &listener_session_id,
                    payload,
                );
            }),
        );
        subscriptions.insert(session_id, subscription);
        return;
    }

    if matches!(
        status.as_str(),
        "completed" | "failed" | "timeout" | "missing"
    ) {
        subscriptions
            .lock()
            .expect("background command subscriptions lock")
            .remove(&session_id);
    }
}

fn handle_background_command_terminal(
    listeners: &Arc<Mutex<BTreeMap<SessionListenerId, SessionEventHandler>>>,
    steering_handle: &SessionSteeringHandle,
    session_id: &str,
    payload: &Value,
) {
    let notification_message = build_background_command_notification(payload);
    let queued_to_running_session = steering_handle.steer(notification_message.clone()).is_ok();
    let mut event_payload = payload
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    event_payload.insert(
        "session_id".to_string(),
        Value::String(session_id.to_string()),
    );
    event_payload.insert(
        "notification_message".to_string(),
        Value::String(notification_message),
    );
    event_payload.insert(
        "queued_to_running_session".to_string(),
        Value::Bool(queued_to_running_session),
    );

    let status = event_payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("terminal")
        .trim()
        .to_ascii_lowercase();
    emit_session_event(
        listeners,
        &format!("background_command_{status}"),
        event_payload.clone(),
    );
    emit_session_event(listeners, "background_command_terminal", event_payload);
}

fn build_background_command_notification(payload: &Value) -> String {
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let status_text = match status.as_str() {
        "completed" => "completed",
        "failed" => "failed",
        "timeout" => "timed out",
        _ if !status.is_empty() => status.as_str(),
        _ => "updated",
    };
    let session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let command = payload
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let output = payload
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let exit_code = payload.get("exit_code").cloned().unwrap_or(Value::Null);
    let mut summary = if output.is_empty() {
        format!("exit_code={exit_code}")
    } else {
        output.to_string()
    };
    if summary.len() > 500 {
        summary.truncate(497);
        summary = format!("{}...", summary.trim_end());
    }

    let mut lines = vec![format!(
        "System notification: background command {session_id} {status_text}."
    )];
    if !command.is_empty() {
        lines.push(format!("Command: {command}"));
    }
    if !summary.is_empty() {
        lines.push(format!("Summary: {summary}"));
    }
    lines.join("\n")
}

fn normalize_session_prompt(prompt: String, label: &str) -> Result<String, String> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err(format!("{label} cannot be empty"));
    }
    Ok(prompt.to_string())
}

fn absolutize_workspace(path: PathBuf) -> PathBuf {
    let path = expand_user_path(path);
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir()
        .map(|current_dir| current_dir.join(&path))
        .unwrap_or(path)
}

fn expand_user_path(path: PathBuf) -> PathBuf {
    let raw_path = path.to_string_lossy();
    if raw_path == "~" {
        return home_dir().unwrap_or(path);
    }
    if let Some(rest) = raw_path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    path
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            let mut home = PathBuf::from(drive);
            home.push(path);
            Some(home)
        })
}

fn generate_session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let counter = SESSION_ID_COUNTER.fetch_add(1, Ordering::Relaxed) as u128;
    format!("{:012x}", (nanos ^ counter) & 0xffff_ffff_ffff)
}

fn normalize_session_id(session_id: impl Into<String>) -> String {
    let session_id = session_id.into().trim().to_string();
    if session_id.is_empty() {
        generate_session_id()
    } else {
        session_id
    }
}
