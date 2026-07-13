use super::*;

impl InteractiveSessionSubscription {
    pub(super) fn new(
        receiver: broadcast::Receiver<InteractiveSessionEvent>,
        closed: Arc<AtomicBool>,
    ) -> Self {
        Self { receiver, closed }
    }

    pub async fn recv(&mut self) -> Result<InteractiveSessionEvent, InteractiveSessionError> {
        if self.closed.load(Ordering::SeqCst) && self.receiver.is_empty() {
            return Err(InteractiveSessionError::EventStreamClosed);
        }
        self.receiver.recv().await.map_err(|error| match error {
            broadcast::error::RecvError::Closed => InteractiveSessionError::EventStreamClosed,
            broadcast::error::RecvError::Lagged(missed) => {
                InteractiveSessionError::EventGap { missed }
            }
        })
    }

    pub fn try_recv(&mut self) -> Result<InteractiveSessionEvent, InteractiveSessionError> {
        self.receiver.try_recv().map_err(|error| match error {
            broadcast::error::TryRecvError::Closed => InteractiveSessionError::EventStreamClosed,
            broadcast::error::TryRecvError::Empty if self.closed.load(Ordering::SeqCst) => {
                InteractiveSessionError::EventStreamClosed
            }
            broadcast::error::TryRecvError::Empty => InteractiveSessionError::EventStreamEmpty,
            broadcast::error::TryRecvError::Lagged(missed) => {
                InteractiveSessionError::EventGap { missed }
            }
        })
    }
}

pub(super) struct InteractiveRunHandleController {
    pub(super) inner: std::sync::Weak<InteractiveSessionInner>,
    pub(super) session_id: String,
}

impl InteractiveRunHandleController {
    fn session(&self) -> Result<InteractiveSession, String> {
        self.inner
            .upgrade()
            .map(|inner| InteractiveSession { inner })
            .ok_or_else(|| format!("interactive session `{}` is closed", self.session_id))
    }
}

impl RunHandleController for InteractiveRunHandleController {
    fn steer(&self, message: String) -> Result<(), String> {
        self.session()
            .and_then(|session| session.steer(message).map_err(|error| error.to_string()))
    }

    fn follow_up(&self, message: String) -> Result<(), String> {
        self.session().and_then(|session| {
            session
                .follow_up(message)
                .map_err(|error| error.to_string())
        })
    }
}

pub(super) struct RunEventForwarder {
    task: Option<tokio::task::JoinHandle<()>>,
}

impl RunEventForwarder {
    pub(super) fn new(task: tokio::task::JoinHandle<()>) -> Self {
        Self { task: Some(task) }
    }

    pub(super) async fn finish(&mut self) -> Result<(), tokio::task::JoinError> {
        let result = match self.task.as_mut() {
            Some(task) => task.await,
            None => Ok(()),
        };
        self.task.take();
        result
    }
}

impl Drop for RunEventForwarder {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub(super) struct RunLifecycleGuard {
    inner: Arc<InteractiveSessionInner>,
    armed: bool,
}

impl RunLifecycleGuard {
    pub(super) fn begin(
        inner: Arc<InteractiveSessionInner>,
    ) -> Result<Self, InteractiveSessionError> {
        {
            let mut state = lock_unpoisoned(&inner.state);
            if state.closed {
                return Err(InteractiveSessionError::Closed {
                    session_id: inner.session_id.clone(),
                });
            }
            if state.running {
                return Err(InteractiveSessionError::AlreadyRunning {
                    session_id: inner.session_id.clone(),
                });
            }
            state.running = true;
            state.active_cancellation_token = Some(
                inner
                    .run_config
                    .cancellation_token
                    .as_ref()
                    .map(CancellationToken::child)
                    .unwrap_or_default(),
            );
        }
        Ok(Self { inner, armed: true })
    }

    pub(super) fn finish(&mut self) {
        self.reset(false);
        self.armed = false;
    }

    fn reset(&self, aborted: bool) {
        let (active, controller) = {
            let mut state = lock_unpoisoned(&self.inner.state);
            state.running = false;
            state.active_cancellation_token = None;
            (
                state.active_handle.take(),
                state.active_handle_controller.take(),
            )
        };
        if let Some(handle) = active {
            if let Some(controller) = controller {
                handle.detach_controller(controller);
            }
            handle.cancel_with_reason("interactive session run aborted");
            let _ = self
                .inner
                .events
                .send(InteractiveSessionEvent::ActiveHandleChanged {
                    session_id: self.inner.session_id.clone(),
                    active: false,
                });
        }
        if aborted && !self.inner.closed.load(Ordering::SeqCst) {
            let _ = self.inner.events.send(InteractiveSessionEvent::RunAborted {
                session_id: self.inner.session_id.clone(),
            });
        }
    }
}

impl Drop for RunLifecycleGuard {
    fn drop(&mut self) {
        if self.armed {
            self.reset(true);
        }
    }
}

pub(super) struct SteeringRuntimeHook {
    pub(super) queue: SteeringQueue,
    pub(super) session_id: String,
    pub(super) events: broadcast::Sender<InteractiveSessionEvent>,
    pub(super) inner: std::sync::Weak<InteractiveSessionInner>,
}

impl RuntimeHook for SteeringRuntimeHook {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        let queued = {
            let mut queue = lock_queue(&self.queue);
            queue.drain(..).collect::<Vec<_>>()
        };
        if queued.is_empty() {
            return None;
        }
        let mut messages = event.messages.to_vec();
        for prompt in queued {
            messages.push(Message::user(prompt.clone()));
            let _ = self.events.send(InteractiveSessionEvent::SteerDequeued {
                session_id: self.session_id.clone(),
                prompt,
                cycle_index: Some(event.cycle_index),
            });
        }
        Some(BeforeLlmPatch {
            messages: Some(messages),
            tool_schemas: None,
        })
    }

    fn before_tool_call(&self, event: BeforeToolCallEvent<'_>) -> Option<BeforeToolCallPatch> {
        if lock_queue(&self.queue).is_empty() {
            return None;
        }
        let mut result = ToolExecutionResult::error(
            event.call.id.clone(),
            serde_json::json!({
                "ok": false,
                "error": "Tool skipped due to queued steering message.",
                "error_code": "skipped_due_to_steering",
            })
            .to_string(),
        );
        result.error_code = Some("skipped_due_to_steering".to_string());
        Some(result.into())
    }

    fn after_tool_call(&self, event: AfterToolCallEvent<'_>) -> Option<ToolExecutionResult> {
        if let Some(inner) = self.inner.upgrade() {
            InteractiveSession { inner }
                .sync_background_command_result(&event.call.name, event.result);
        }
        None
    }
}

pub(super) fn build_background_command_notification(payload: &Value) -> String {
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
        "" => "updated",
        other => other,
    };
    let background_session_id = payload
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
    let mut summary = if output.is_empty() {
        payload
            .get("exit_code")
            .map(|exit_code| format!("exit_code={exit_code}"))
            .unwrap_or_default()
    } else {
        output.to_string()
    };
    if summary.chars().count() > 500 {
        summary = format!(
            "{}...",
            summary.chars().take(497).collect::<String>().trim_end()
        );
    }

    let mut lines = vec![format!(
        "System notification: background command {background_session_id} {status_text}."
    )];
    if !command.is_empty() {
        lines.push(format!("Command: {command}"));
    }
    if !summary.is_empty() {
        lines.push(format!("Summary: {summary}"));
    }
    lines.join("\n")
}

pub(super) fn resolve_session(
    requested_id: Option<String>,
    session: Option<Arc<dyn Session>>,
) -> Result<(String, Arc<dyn Session>), InteractiveSessionError> {
    let requested_id = requested_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    match session {
        Some(session) => {
            let actual = session.session_id().trim().to_string();
            if actual.is_empty() {
                return Err(InteractiveSessionError::EmptySessionId);
            }
            if let Some(requested) = requested_id.as_ref() {
                if requested != &actual {
                    return Err(InteractiveSessionError::SessionIdMismatch {
                        requested: requested.clone(),
                        actual,
                    });
                }
            }
            Ok((requested_id.unwrap_or(actual), session))
        }
        None => {
            let session_id = requested_id.unwrap_or_else(new_session_id);
            let session: Arc<dyn Session> = Arc::new(MemorySession::new(session_id.clone()));
            Ok((session_id, session))
        }
    }
}

pub(super) fn select_session_source(
    option_session: Option<Arc<dyn Session>>,
    run_config_session: Option<Arc<dyn Session>>,
) -> Result<Option<Arc<dyn Session>>, InteractiveSessionError> {
    match (option_session, run_config_session) {
        (Some(option), Some(configured)) => {
            let option_id = option.session_id().trim();
            let configured_id = configured.session_id().trim();
            if option_id != configured_id {
                return Err(InteractiveSessionError::SessionIdMismatch {
                    requested: option_id.to_string(),
                    actual: configured_id.to_string(),
                });
            }
            Ok(Some(option))
        }
        (Some(session), None) | (None, Some(session)) => Ok(Some(session)),
        (None, None) => Ok(None),
    }
}

pub(super) fn normalized_prompt(
    prompt: impl Into<String>,
    operation: &'static str,
) -> Result<String, InteractiveSessionError> {
    let prompt = prompt.into().trim().to_string();
    if prompt.is_empty() {
        return Err(InteractiveSessionError::EmptyPrompt { operation });
    }
    Ok(prompt)
}

pub(super) fn new_session_id() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("session_{}", &id[..12])
}

pub(super) fn lock_queue(queue: &SteeringQueue) -> MutexGuard<'_, VecDeque<String>> {
    lock_unpoisoned(queue)
}

pub(super) fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}
