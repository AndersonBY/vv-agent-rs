use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::app_server::protocol::AppTurn;
use crate::app_server::transport::ConnectionId;
use crate::RunHandle;

#[derive(Clone, Default)]
pub struct ThreadStateManager {
    inner: Arc<Mutex<ThreadStateInner>>,
}

#[derive(Default)]
struct ThreadStateInner {
    subscribers: HashMap<String, HashSet<ConnectionId>>,
    active_turns: HashMap<String, ActiveTurn>,
    pending_approvals: HashMap<String, PendingApproval>,
}

#[derive(Clone)]
pub struct ActiveTurn {
    pub turn: AppTurn,
    pub handle: RunHandle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    pub turn_id: String,
    pub request_id: String,
}

impl ThreadStateManager {
    pub async fn subscribe(&self, thread_id: impl Into<String>, connection_id: ConnectionId) {
        self.inner
            .lock()
            .await
            .subscribers
            .entry(thread_id.into())
            .or_default()
            .insert(connection_id);
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

    pub async fn set_active_turn(&self, thread_id: impl Into<String>, active_turn: ActiveTurn) {
        self.inner
            .lock()
            .await
            .active_turns
            .insert(thread_id.into(), active_turn);
    }

    pub async fn active_turn(&self, thread_id: &str) -> Option<ActiveTurn> {
        self.inner.lock().await.active_turns.get(thread_id).cloned()
    }

    pub async fn clear_active_turn(&self, thread_id: &str, turn_id: &str) {
        let mut inner = self.inner.lock().await;
        if inner
            .active_turns
            .get(thread_id)
            .is_some_and(|active| active.turn.id == turn_id)
        {
            inner.active_turns.remove(thread_id);
        }
    }

    pub async fn cancel_turn(&self, thread_id: &str, turn_id: &str) -> bool {
        let active = self.active_turn(thread_id).await;
        let Some(active) = active else {
            return false;
        };
        if active.turn.id != turn_id {
            return false;
        }
        active.handle.cancel();
        true
    }

    pub async fn set_pending_approval(
        &self,
        thread_id: impl Into<String>,
        turn_id: impl Into<String>,
        request_id: impl Into<String>,
    ) {
        self.inner.lock().await.pending_approvals.insert(
            thread_id.into(),
            PendingApproval {
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
