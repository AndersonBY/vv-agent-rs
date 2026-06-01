use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::approval::{ApprovalBroker, ApprovalError};
use crate::events::RunEvent;
use crate::result::RunResult;
use crate::runner::RunEventStream;
use crate::runtime::CancellationToken;
use crate::tools::ApprovalDecision;

pub(crate) type RunEventSenderSlot = Arc<Mutex<Option<broadcast::Sender<RunEvent>>>>;
type RunJoinHandle = JoinHandle<Result<RunResult, String>>;
type SharedJoinHandle = Arc<tokio::sync::Mutex<Option<RunJoinHandle>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunHandleStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
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

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            status: RunHandleStatus::Failed,
            done: true,
            cancelled: false,
            error: Some(error.into()),
        }
    }

    pub fn cancelled() -> Self {
        Self {
            status: RunHandleStatus::Cancelled,
            done: true,
            cancelled: true,
            error: Some("run cancelled".to_string()),
        }
    }
}

#[derive(Clone)]
pub(crate) struct SharedRunResult {
    join: SharedJoinHandle,
}

impl SharedRunResult {
    pub(crate) fn new(join: RunJoinHandle) -> Self {
        Self {
            join: Arc::new(tokio::sync::Mutex::new(Some(join))),
        }
    }

    pub(crate) async fn wait(&self) -> Result<RunResult, String> {
        let join = self
            .join
            .lock()
            .await
            .take()
            .ok_or_else(|| "run result already taken".to_string())?;
        join.await
            .map_err(|error| format!("run task failed: {error}"))?
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
}

impl RunHandle {
    pub(crate) fn new(
        sender: RunEventSenderSlot,
        events: Arc<Mutex<Vec<RunEvent>>>,
        result: SharedRunResult,
        state: Arc<Mutex<RunHandleState>>,
        cancellation_token: CancellationToken,
        approval_broker: ApprovalBroker,
    ) -> Self {
        Self {
            sender,
            events,
            result,
            state,
            cancellation_token,
            approval_broker,
        }
    }

    pub fn events(&self) -> RunEventStream {
        let receiver = self
            .sender
            .lock()
            .ok()
            .and_then(|sender| sender.as_ref().map(broadcast::Sender::subscribe));
        RunEventStream::from_live(receiver, Some(self.result.clone()), self.event_snapshot())
    }

    pub(crate) fn into_event_stream(self) -> RunEventStream {
        let receiver = self
            .sender
            .lock()
            .ok()
            .and_then(|sender| sender.as_ref().map(broadcast::Sender::subscribe));
        RunEventStream::from_live(receiver, Some(self.result.clone()), self.event_snapshot())
    }

    pub async fn result(&self) -> Result<RunResult, String> {
        self.result.wait().await
    }

    pub fn state(&self) -> RunHandleState {
        self.state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_else(|_| RunHandleState::failed("run handle state lock poisoned"))
    }

    pub fn cancel(&self) {
        self.cancellation_token.cancel();
        if let Ok(mut state) = self.state.lock() {
            if !state.done {
                *state = RunHandleState::cancelled();
            }
        }
    }

    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
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

    fn event_snapshot(&self) -> Vec<RunEvent> {
        self.events
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default()
    }
}
