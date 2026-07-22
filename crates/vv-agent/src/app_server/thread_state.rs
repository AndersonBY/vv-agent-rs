use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::Mutex;

use crate::app_server::protocol::{AppTurn, UserInput};
use crate::app_server::transport::ConnectionId;
use crate::runtime::state::CheckpointStore;
use crate::RunHandle;

pub type SteeringQueue = Arc<StdMutex<VecDeque<Vec<UserInput>>>>;

#[derive(Clone, Default)]
pub struct ThreadStateManager {
    inner: Arc<Mutex<ThreadStateInner>>,
}

#[derive(Default)]
struct ThreadStateInner {
    subscribers: HashMap<String, HashSet<ConnectionId>>,
    active_turns: HashMap<String, ActiveTurn>,
    durable_resumes: HashMap<String, String>,
    pending_approvals: HashMap<String, PendingApproval>,
    follow_ups: HashMap<String, VecDeque<Vec<UserInput>>>,
    closed_threads: HashSet<String>,
}

#[derive(Clone)]
pub struct ActiveTurn {
    pub turn: AppTurn,
    pub handle: RunHandle,
    pub steering: SteeringQueue,
    pub owner_connection_id: ConnectionId,
    pub checkpoint_store: Option<Arc<dyn CheckpointStore>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    pub connection_id: ConnectionId,
    pub turn_id: String,
    pub request_id: String,
}

impl ThreadStateManager {
    pub async fn subscribe(&self, thread_id: impl Into<String>, connection_id: ConnectionId) {
        let thread_id = thread_id.into();
        let mut inner = self.inner.lock().await;
        inner.closed_threads.remove(&thread_id);
        inner
            .subscribers
            .entry(thread_id)
            .or_default()
            .insert(connection_id);
    }

    pub async fn subscribe_and_snapshot<T, E>(
        &self,
        thread_id: impl Into<String>,
        connection_id: ConnectionId,
        snapshot: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let thread_id = thread_id.into();
        let mut inner = self.inner.lock().await;
        let was_closed = inner.closed_threads.remove(&thread_id);
        let inserted = inner
            .subscribers
            .entry(thread_id.clone())
            .or_default()
            .insert(connection_id);
        let result = snapshot();
        if result.is_err() {
            if inserted {
                let remove_entry =
                    inner
                        .subscribers
                        .get_mut(&thread_id)
                        .is_some_and(|subscribers| {
                            subscribers.remove(&connection_id);
                            subscribers.is_empty()
                        });
                if remove_entry {
                    inner.subscribers.remove(&thread_id);
                }
            }
            if was_closed {
                inner.closed_threads.insert(thread_id);
            }
        }
        result
    }

    pub async fn unsubscribe(&self, thread_id: &str, connection_id: ConnectionId) -> bool {
        let mut inner = self.inner.lock().await;
        if let Some(subscribers) = inner.subscribers.get_mut(thread_id) {
            subscribers.remove(&connection_id);
            if subscribers.is_empty() {
                inner.subscribers.remove(thread_id);
            }
        }
        let closed = !inner.subscribers.contains_key(thread_id)
            && !inner.active_turns.contains_key(thread_id)
            && !inner.durable_resumes.contains_key(thread_id);
        if closed {
            inner.closed_threads.insert(thread_id.to_string());
        }
        closed
    }

    pub async fn unsubscribe_connection(&self, connection_id: ConnectionId) {
        let mut inner = self.inner.lock().await;
        inner.subscribers.retain(|_thread_id, subscribers| {
            subscribers.remove(&connection_id);
            !subscribers.is_empty()
        });
    }

    pub async fn subscribers(&self, thread_id: &str) -> Vec<ConnectionId> {
        self.inner
            .lock()
            .await
            .subscribers
            .get(thread_id)
            .map(|subscribers| subscribers.iter().copied().collect())
            .unwrap_or_default()
    }

    pub async fn is_subscribed(&self, thread_id: &str, connection_id: ConnectionId) -> bool {
        self.inner
            .lock()
            .await
            .subscribers
            .get(thread_id)
            .is_some_and(|subscribers| subscribers.contains(&connection_id))
    }

    pub async fn is_closed(&self, thread_id: &str) -> bool {
        self.inner.lock().await.closed_threads.contains(thread_id)
    }

    pub async fn reopen(&self, thread_id: &str) {
        self.inner.lock().await.closed_threads.remove(thread_id);
    }

    pub async fn set_active_turn(&self, thread_id: impl Into<String>, active_turn: ActiveTurn) {
        let thread_id = thread_id.into();
        let mut inner = self.inner.lock().await;
        inner.closed_threads.remove(&thread_id);
        inner.active_turns.insert(thread_id, active_turn);
    }

    pub async fn active_turn(&self, thread_id: &str) -> Option<ActiveTurn> {
        self.inner.lock().await.active_turns.get(thread_id).cloned()
    }

    pub async fn active_turn_id(&self, thread_id: &str) -> Option<String> {
        let inner = self.inner.lock().await;
        inner
            .active_turns
            .get(thread_id)
            .map(|active| active.turn.turn_id.clone())
            .or_else(|| inner.durable_resumes.get(thread_id).cloned())
    }

    pub async fn has_active_turn(&self, thread_id: &str) -> bool {
        let inner = self.inner.lock().await;
        inner.active_turns.contains_key(thread_id) || inner.durable_resumes.contains_key(thread_id)
    }

    pub async fn set_durable_resume(
        &self,
        thread_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) {
        let thread_id = thread_id.into();
        let mut inner = self.inner.lock().await;
        inner.closed_threads.remove(&thread_id);
        inner.durable_resumes.insert(thread_id, turn_id.into());
    }

    pub async fn clear_durable_resume(&self, thread_id: &str, turn_id: &str) {
        let mut inner = self.inner.lock().await;
        if inner
            .durable_resumes
            .get(thread_id)
            .is_some_and(|active_turn_id| active_turn_id == turn_id)
        {
            inner.durable_resumes.remove(thread_id);
        }
    }

    pub async fn clear_active_turn(&self, thread_id: &str, turn_id: &str) {
        let mut inner = self.inner.lock().await;
        if inner
            .active_turns
            .get(thread_id)
            .is_some_and(|active| active.turn.turn_id == turn_id)
        {
            inner.active_turns.remove(thread_id);
        }
    }

    pub async fn queue_follow_up(&self, thread_id: &str, input: Vec<UserInput>) {
        self.inner
            .lock()
            .await
            .follow_ups
            .entry(thread_id.to_string())
            .or_default()
            .push_back(input);
    }

    pub async fn pop_follow_up(&self, thread_id: &str) -> Option<Vec<UserInput>> {
        let mut inner = self.inner.lock().await;
        let next = inner
            .follow_ups
            .get_mut(thread_id)
            .and_then(VecDeque::pop_front);
        if inner
            .follow_ups
            .get(thread_id)
            .is_some_and(VecDeque::is_empty)
        {
            inner.follow_ups.remove(thread_id);
        }
        next
    }

    pub async fn set_pending_approval(
        &self,
        thread_id: impl Into<String>,
        turn_id: impl Into<String>,
        request_id: impl Into<String>,
        connection_id: ConnectionId,
    ) {
        self.inner.lock().await.pending_approvals.insert(
            thread_id.into(),
            PendingApproval {
                connection_id,
                turn_id: turn_id.into(),
                request_id: request_id.into(),
            },
        );
    }

    pub async fn pending_approval(&self, thread_id: &str) -> Option<PendingApproval> {
        self.inner
            .lock()
            .await
            .pending_approvals
            .get(thread_id)
            .cloned()
    }

    pub async fn clear_pending_approval(&self, thread_id: &str, request_id: &str) {
        let mut inner = self.inner.lock().await;
        if inner
            .pending_approvals
            .get(thread_id)
            .is_some_and(|pending| pending.request_id == request_id)
        {
            inner.pending_approvals.remove(thread_id);
        }
    }
}
