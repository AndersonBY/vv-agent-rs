use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{Mutex, Notify};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RequestSerializationQueueKey(String);

impl RequestSerializationQueueKey {
    pub fn thread(thread_id: impl Into<String>) -> Self {
        Self(format!("thread:{}", thread_id.into()))
    }

    pub fn global(name: impl Into<String>) -> Self {
        Self(format!("global:{}", name.into()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestSerializationAccess {
    Shared,
    Exclusive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSerializationScope {
    key: RequestSerializationQueueKey,
    access: RequestSerializationAccess,
}

impl RequestSerializationScope {
    pub fn shared_thread(thread_id: impl Into<String>) -> Self {
        Self {
            key: RequestSerializationQueueKey::thread(thread_id),
            access: RequestSerializationAccess::Shared,
        }
    }

    pub fn exclusive_thread(thread_id: impl Into<String>) -> Self {
        Self {
            key: RequestSerializationQueueKey::thread(thread_id),
            access: RequestSerializationAccess::Exclusive,
        }
    }

    pub fn shared_global(name: impl Into<String>) -> Self {
        Self {
            key: RequestSerializationQueueKey::global(name),
            access: RequestSerializationAccess::Shared,
        }
    }

    pub fn exclusive_global(name: impl Into<String>) -> Self {
        Self {
            key: RequestSerializationQueueKey::global(name),
            access: RequestSerializationAccess::Exclusive,
        }
    }

    pub fn for_method(method: &str, params: Option<&Value>) -> Option<Self> {
        match method {
            "thread/start" => Some(Self::exclusive_global("thread")),
            "thread/resume" | "thread/read" => thread_id(params).map(Self::shared_thread),
            "thread/archive" | "thread/unsubscribe" | "turn/start" | "turn/interrupt"
            | "turn/steer" | "turn/followUp" | "approval/resolve" => {
                thread_id(params).map(Self::exclusive_thread)
            }
            "thread/list" => Some(Self::shared_global("thread/list")),
            "model/list" => Some(Self::shared_global("model")),
            "schema/export" => Some(Self::shared_global("schema")),
            _ => None,
        }
    }

    pub fn key(&self) -> &RequestSerializationQueueKey {
        &self.key
    }

    pub fn access(&self) -> RequestSerializationAccess {
        self.access
    }
}

#[derive(Clone, Default)]
pub struct RequestSerializationQueue {
    inner: Arc<Mutex<HashMap<RequestSerializationQueueKey, QueueState>>>,
}

impl RequestSerializationQueue {
    pub async fn run<F, T>(&self, scope: RequestSerializationScope, future: F) -> T
    where
        F: Future<Output = T>,
    {
        self.acquire(scope.clone()).await;
        let result = future.await;
        self.release(scope).await;
        result
    }

    async fn acquire(&self, scope: RequestSerializationScope) {
        let notify = Arc::new(Notify::new());
        let mut queues = self.inner.lock().await;
        let state = queues.entry(scope.key.clone()).or_default();

        if state.can_start_now(scope.access) {
            state.start(scope.access);
            return;
        }

        state.waiters.push_back(QueuedRequest {
            access: scope.access,
            notify: notify.clone(),
        });
        drop(queues);
        notify.notified().await;
    }

    async fn release(&self, scope: RequestSerializationScope) {
        let mut queues = self.inner.lock().await;
        let Some(state) = queues.get_mut(&scope.key) else {
            return;
        };
        state.finish(scope.access);
        state.wake_next();
        if state.is_empty() {
            queues.remove(&scope.key);
        }
    }
}

#[derive(Default)]
struct QueueState {
    active_shared: usize,
    active_exclusive: bool,
    waiters: VecDeque<QueuedRequest>,
}

impl QueueState {
    fn can_start_now(&self, access: RequestSerializationAccess) -> bool {
        self.waiters.is_empty()
            && match access {
                RequestSerializationAccess::Shared => !self.active_exclusive,
                RequestSerializationAccess::Exclusive => {
                    !self.active_exclusive && self.active_shared == 0
                }
            }
    }

    fn start(&mut self, access: RequestSerializationAccess) {
        match access {
            RequestSerializationAccess::Shared => self.active_shared += 1,
            RequestSerializationAccess::Exclusive => self.active_exclusive = true,
        }
    }

    fn finish(&mut self, access: RequestSerializationAccess) {
        match access {
            RequestSerializationAccess::Shared => {
                self.active_shared = self.active_shared.saturating_sub(1);
            }
            RequestSerializationAccess::Exclusive => {
                self.active_exclusive = false;
            }
        }
    }

    fn wake_next(&mut self) {
        if self.active_exclusive || self.active_shared > 0 {
            return;
        }

        match self.waiters.front().map(|waiter| waiter.access) {
            Some(RequestSerializationAccess::Exclusive) => {
                if let Some(waiter) = self.waiters.pop_front() {
                    self.active_exclusive = true;
                    waiter.notify.notify_one();
                }
            }
            Some(RequestSerializationAccess::Shared) => {
                while self
                    .waiters
                    .front()
                    .is_some_and(|waiter| waiter.access == RequestSerializationAccess::Shared)
                {
                    if let Some(waiter) = self.waiters.pop_front() {
                        self.active_shared += 1;
                        waiter.notify.notify_one();
                    }
                }
            }
            None => {}
        }
    }

    fn is_empty(&self) -> bool {
        self.active_shared == 0 && !self.active_exclusive && self.waiters.is_empty()
    }
}

struct QueuedRequest {
    access: RequestSerializationAccess,
    notify: Arc<Notify>,
}

fn thread_id(params: Option<&Value>) -> Option<String> {
    params
        .and_then(|params| params.get("threadId"))
        .and_then(Value::as_str)
        .map(str::to_string)
}
