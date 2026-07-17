use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::approval::{ApprovalBroker, ApprovalError};
use crate::events::{RunEvent, RunEventPayload};
use crate::result::{RunResult, RunState};
use crate::runner::{NormalizedInput, RunEventStream};
use crate::runtime::CancellationToken;
use crate::tools::ApprovalDecision;
use crate::types::AgentStatus;

pub(crate) type RunEventSenderSlot = Arc<Mutex<Option<broadcast::Sender<RunEvent>>>>;
pub(crate) type RunCompletionSignal = tokio::sync::watch::Receiver<bool>;
type RunJoinHandle = JoinHandle<Result<RunResult, String>>;
type SharedJoinHandle = Arc<tokio::sync::Mutex<Option<RunJoinHandle>>>;
type CachedRunResult = Arc<tokio::sync::OnceCell<Result<RunResult, String>>>;

pub(crate) trait RunHandleController: Send + Sync {
    fn steer(&self, message: String) -> Result<(), String>;
    fn follow_up(&self, message: String) -> Result<(), String>;
}

#[derive(Default)]
struct ControllerBinding {
    generation: u64,
    active: Option<(u64, Arc<dyn RunHandleController>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunHandleStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    WaitUser,
    MaxCycles,
    ReconciliationRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunHandleState {
    pub status: RunHandleStatus,
    pub done: bool,
    pub cancelled: bool,
    pub error: Option<String>,
}

impl RunHandleState {
    pub fn running() -> Self {
        Self {
            status: RunHandleStatus::Running,
            done: false,
            cancelled: false,
            error: None,
        }
    }

    pub fn completed() -> Self {
        Self {
            status: RunHandleStatus::Completed,
            done: true,
            cancelled: false,
            error: None,
        }
    }

    pub fn from_agent_status(status: AgentStatus) -> Self {
        match status {
            AgentStatus::Completed => Self::completed(),
            AgentStatus::WaitUser => Self {
                status: RunHandleStatus::WaitUser,
                done: true,
                cancelled: false,
                error: None,
            },
            AgentStatus::MaxCycles => Self {
                status: RunHandleStatus::MaxCycles,
                done: true,
                cancelled: false,
                error: None,
            },
            AgentStatus::Failed => Self {
                status: RunHandleStatus::Failed,
                done: true,
                cancelled: false,
                error: None,
            },
            AgentStatus::ReconciliationRequired => Self {
                status: RunHandleStatus::ReconciliationRequired,
                done: true,
                cancelled: false,
                error: None,
            },
            AgentStatus::Pending | AgentStatus::Running => Self::running(),
        }
    }

    pub(crate) fn from_run_result(result: &RunResult) -> Self {
        match result.status() {
            AgentStatus::Failed => Self {
                status: RunHandleStatus::Failed,
                done: true,
                cancelled: false,
                error: result.result().error.clone(),
            },
            status => Self::from_agent_status(status),
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            status: RunHandleStatus::Failed,
            done: true,
            cancelled: false,
            error: Some(error.into()),
        }
    }

    pub fn cancelled() -> Self {
        Self::cancelled_with_reason("run cancelled")
    }

    pub fn cancelled_with_reason(reason: impl Into<String>) -> Self {
        Self {
            status: RunHandleStatus::Cancelled,
            done: true,
            cancelled: true,
            error: Some(reason.into()),
        }
    }
}

#[derive(Clone)]
pub(crate) struct SharedRunResult {
    join: SharedJoinHandle,
    cached: CachedRunResult,
}

impl SharedRunResult {
    pub(crate) fn new(join: RunJoinHandle) -> Self {
        Self {
            join: Arc::new(tokio::sync::Mutex::new(Some(join))),
            cached: Arc::new(tokio::sync::OnceCell::new()),
        }
    }

    pub(crate) async fn wait(&self) -> Result<RunResult, String> {
        self.cached
            .get_or_init(|| async {
                let join = self
                    .join
                    .lock()
                    .await
                    .take()
                    .ok_or_else(|| "run result task is unavailable".to_string())?;
                join.await
                    .map_err(|error| format!("run task failed: {error}"))?
            })
            .await
            .clone()
    }
}

#[derive(Clone)]
pub struct RunHandle {
    sender: RunEventSenderSlot,
    events: Arc<Mutex<Vec<RunEvent>>>,
    result: SharedRunResult,
    state: Arc<Mutex<RunHandleState>>,
    cancellation_token: CancellationToken,
    approval_broker: ApprovalBroker,
    completion: RunCompletionSignal,
    cancel_requested: Arc<AtomicBool>,
    controller: Arc<Mutex<ControllerBinding>>,
}

impl RunHandle {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        sender: RunEventSenderSlot,
        events: Arc<Mutex<Vec<RunEvent>>>,
        result: SharedRunResult,
        state: Arc<Mutex<RunHandleState>>,
        cancellation_token: CancellationToken,
        approval_broker: ApprovalBroker,
        completion: RunCompletionSignal,
        cancel_requested: Arc<AtomicBool>,
    ) -> Self {
        Self {
            sender,
            events,
            result,
            state,
            cancellation_token,
            approval_broker,
            completion,
            cancel_requested,
            controller: Arc::new(Mutex::new(ControllerBinding::default())),
        }
    }

    pub fn events(&self) -> RunEventStream {
        let receiver = self
            .sender
            .lock()
            .ok()
            .and_then(|sender| sender.as_ref().map(broadcast::Sender::subscribe));
        RunEventStream::from_live(
            receiver,
            Some(self.result.clone()),
            self.events.clone(),
            self.completion.clone(),
        )
    }

    pub(crate) fn into_event_stream(self) -> RunEventStream {
        let receiver = self
            .sender
            .lock()
            .ok()
            .and_then(|sender| sender.as_ref().map(broadcast::Sender::subscribe));
        RunEventStream::from_live(
            receiver,
            Some(self.result.clone()),
            self.events.clone(),
            self.completion.clone(),
        )
    }

    pub async fn result(&self) -> Result<RunResult, String> {
        self.result.wait().await
    }

    pub fn state(&self) -> RunHandleState {
        let events = self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let has_active_sub_runs = !active_sub_run_ids(&events).is_empty();
        drop(events);
        let mut state = self
            .state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_else(|_| RunHandleState::failed("run handle state lock poisoned"));
        if has_active_sub_runs {
            return RunHandleState {
                status: RunHandleStatus::Running,
                done: false,
                cancelled: self.cancel_requested.load(Ordering::SeqCst)
                    || self.cancellation_token.is_cancelled(),
                error: None,
            };
        }
        if !state.done
            && (self.cancel_requested.load(Ordering::SeqCst)
                || self.cancellation_token.is_cancelled())
        {
            state.cancelled = true;
        }
        if state.done && self.cancel_requested.load(Ordering::SeqCst) {
            return RunHandleState::cancelled_with_reason(
                self.cancellation_token
                    .reason()
                    .unwrap_or_else(|| "Operation was cancelled".to_string()),
            );
        }
        state
    }

    pub fn cancel(&self) -> bool {
        self.cancel_with_reason("Run was cancelled.")
    }

    pub fn cancel_with_reason(&self, reason: impl Into<String>) -> bool {
        let reason = reason.into();
        if self.cancellation_token.is_cancelled() {
            return false;
        }
        let events = self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let active_sub_runs = active_sub_run_ids(&events);
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if (state.done || terminal_event_seen(&events)) && active_sub_runs.is_empty() {
            return false;
        }
        if self
            .cancel_requested
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        drop(state);
        drop(events);
        self.cancellation_token.cancel_with_reason(reason);
        true
    }

    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    pub fn steer(&self, message: impl Into<String>) -> Result<(), String> {
        let message = message.into();
        self.with_controller("steer", |controller| controller.steer(message))
    }

    pub fn follow_up(&self, message: impl Into<String>) -> Result<(), String> {
        let message = message.into();
        self.with_controller("follow_up", |controller| controller.follow_up(message))
    }

    pub async fn resume(&self, state: RunState) -> Result<RunResult, String> {
        let origin_runner = state
            .result()
            .resume_context()
            .map(|context| context.runner.clone())
            .ok_or_else(|| "run state does not include resume context".to_string())?;
        origin_runner.resume(state).await
    }

    pub async fn resume_with_input(
        &self,
        state: RunState,
        input: impl Into<NormalizedInput>,
    ) -> Result<RunResult, String> {
        let origin_runner = state
            .result()
            .resume_context()
            .map(|context| context.runner.clone())
            .ok_or_else(|| "run state does not include resume context".to_string())?;
        origin_runner.resume_with_input(state, input).await
    }

    pub async fn approve(
        &self,
        request_id: impl AsRef<str>,
        decision: ApprovalDecision,
    ) -> Result<(), ApprovalError> {
        self.approval_broker.resolve(request_id, decision)
    }

    pub fn approval_broker(&self) -> &ApprovalBroker {
        &self.approval_broker
    }

    pub(crate) fn attach_controller(&self, controller: Arc<dyn RunHandleController>) -> u64 {
        let mut binding = self
            .controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        binding.generation = binding.generation.wrapping_add(1).max(1);
        let generation = binding.generation;
        binding.active = Some((generation, controller));
        generation
    }

    pub(crate) fn detach_controller(&self, generation: u64) -> bool {
        let mut binding = self
            .controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if binding
            .active
            .as_ref()
            .is_some_and(|(active_generation, _)| *active_generation == generation)
        {
            binding.active = None;
            return true;
        }
        false
    }

    fn with_controller<T>(
        &self,
        method: &str,
        callback: impl FnOnce(&dyn RunHandleController) -> Result<T, String>,
    ) -> Result<T, String> {
        let binding = self
            .controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let controller = binding
            .active
            .as_ref()
            .map(|(_, controller)| controller.as_ref())
            .ok_or_else(|| {
                format!(
                    "RunHandle.{method}() is only available when the handle is attached to an interactive session."
                )
            })?;
        callback(controller)
    }
}

fn terminal_event_seen(events: &[RunEvent]) -> bool {
    let mut active_handoffs = 0usize;
    let mut terminal_seen = false;
    let mut terminal_seen_during_handoff = false;
    for event in events {
        match event.payload() {
            RunEventPayload::HandoffStarted { .. } => {
                active_handoffs += 1;
                terminal_seen = false;
                terminal_seen_during_handoff = false;
            }
            RunEventPayload::HandoffCompleted { .. } => {
                active_handoffs = active_handoffs.saturating_sub(1);
                if event
                    .metadata()
                    .get("chain_continues")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    terminal_seen = false;
                    terminal_seen_during_handoff = false;
                } else if active_handoffs == 0 && terminal_seen_during_handoff {
                    terminal_seen = true;
                    terminal_seen_during_handoff = false;
                }
            }
            RunEventPayload::RunCompleted { .. }
            | RunEventPayload::RunFailed { .. }
            | RunEventPayload::RunCancelled { .. } => {
                if active_handoffs > 0 {
                    terminal_seen_during_handoff = true;
                } else {
                    terminal_seen = true;
                }
            }
            _ => {}
        }
    }
    terminal_seen
}

pub(crate) fn active_sub_run_ids(events: &[RunEvent]) -> std::collections::HashSet<String> {
    let mut active = std::collections::HashSet::new();
    for event in events {
        match event.payload() {
            RunEventPayload::SubRunStarted { .. } => {
                active.insert(event.run_id().to_string());
            }
            RunEventPayload::SubRunCompleted { .. } => {
                active.remove(event.run_id());
            }
            _ => {}
        }
    }
    active
}
