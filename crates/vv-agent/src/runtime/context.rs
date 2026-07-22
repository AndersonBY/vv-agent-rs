use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::approval::{ApprovalBroker, ApprovalProvider};
use crate::events::RunEvent;
use crate::memory::MemoryProvider;

use super::state::CheckpointStore;
use super::{CancellationToken, CancelledError};

pub type RunEventHandler = Arc<dyn Fn(&RunEvent) + Send + Sync + 'static>;

#[derive(Clone, Default)]
pub struct ExecutionContext {
    pub cancellation_token: Option<CancellationToken>,
    pub event_handler: Option<RunEventHandler>,
    pub checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    pub approval_provider: Option<Arc<dyn ApprovalProvider>>,
    pub approval_broker: Option<ApprovalBroker>,
    pub approval_timeout: Option<Duration>,
    pub memory_providers: Vec<Arc<dyn MemoryProvider>>,
    pub app_state: Option<Arc<dyn std::any::Any + Send + Sync>>,
    pub metadata: BTreeMap<String, Value>,
}

impl std::fmt::Debug for ExecutionContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionContext")
            .field("has_cancellation_token", &self.cancellation_token.is_some())
            .field("has_event_handler", &self.event_handler.is_some())
            .field("has_checkpoint_store", &self.checkpoint_store.is_some())
            .field("has_approval_provider", &self.approval_provider.is_some())
            .field("has_approval_broker", &self.approval_broker.is_some())
            .field("memory_provider_count", &self.memory_providers.len())
            .field("has_app_state", &self.app_state.is_some())
            .field("metadata", &self.metadata)
            .finish()
    }
}

impl ExecutionContext {
    pub fn with_cancellation_token(mut self, cancellation_token: CancellationToken) -> Self {
        self.cancellation_token = Some(cancellation_token);
        self
    }

    pub fn with_event_handler(mut self, event_handler: RunEventHandler) -> Self {
        self.event_handler = Some(event_handler);
        self
    }

    pub fn with_checkpoint_store(mut self, checkpoint_store: Arc<dyn CheckpointStore>) -> Self {
        self.checkpoint_store = Some(checkpoint_store);
        self
    }

    pub fn with_approval_provider(mut self, provider: Arc<dyn ApprovalProvider>) -> Self {
        self.approval_provider = Some(provider);
        self
    }

    pub fn with_approval_broker(mut self, broker: ApprovalBroker) -> Self {
        self.approval_broker = Some(broker);
        self
    }

    pub fn with_approval_timeout(mut self, timeout: Duration) -> Self {
        self.approval_timeout = Some(timeout);
        self
    }

    pub fn with_memory_provider(mut self, provider: Arc<dyn MemoryProvider>) -> Self {
        self.memory_providers.push(provider);
        self
    }

    pub fn with_app_state<T>(mut self, app_state: T) -> Self
    where
        T: Send + Sync + 'static,
    {
        self.app_state = Some(Arc::new(app_state));
        self
    }

    pub fn with_app_state_arc(mut self, app_state: Arc<dyn std::any::Any + Send + Sync>) -> Self {
        self.app_state = Some(app_state);
        self
    }

    pub fn with_metadata(mut self, metadata: BTreeMap<String, Value>) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn check_cancelled(&self) -> Result<(), CancelledError> {
        if let Some(token) = &self.cancellation_token {
            token.check()
        } else {
            Ok(())
        }
    }
}
