use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::approval::{ApprovalBroker, ApprovalProvider};
use crate::llm::LlmStreamCallback;

use super::state::StateStore;
use super::CancellationToken;

pub type StreamCallback = LlmStreamCallback;

#[derive(Clone, Default)]
pub struct ExecutionContext {
    pub cancellation_token: Option<CancellationToken>,
    pub stream_callback: Option<StreamCallback>,
    pub state_store: Option<Arc<dyn StateStore>>,
    pub approval_provider: Option<Arc<dyn ApprovalProvider>>,
    pub approval_broker: Option<ApprovalBroker>,
    pub approval_timeout: Option<Duration>,
    pub metadata: BTreeMap<String, Value>,
}

impl std::fmt::Debug for ExecutionContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionContext")
            .field("has_cancellation_token", &self.cancellation_token.is_some())
            .field("has_stream_callback", &self.stream_callback.is_some())
            .field("has_state_store", &self.state_store.is_some())
            .field("has_approval_provider", &self.approval_provider.is_some())
            .field("has_approval_broker", &self.approval_broker.is_some())
            .field("metadata", &self.metadata)
            .finish()
    }
}

impl ExecutionContext {
    pub fn with_cancellation_token(mut self, cancellation_token: CancellationToken) -> Self {
        self.cancellation_token = Some(cancellation_token);
        self
    }

    pub fn with_stream_callback(mut self, stream_callback: StreamCallback) -> Self {
        self.stream_callback = Some(stream_callback);
        self
    }

    pub fn with_state_store(mut self, state_store: Arc<dyn StateStore>) -> Self {
        self.state_store = Some(state_store);
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

    pub fn with_metadata(mut self, metadata: BTreeMap<String, Value>) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn check_cancelled(&self) -> Result<(), String> {
        if let Some(token) = &self.cancellation_token {
            token.check()
        } else {
            Ok(())
        }
    }
}
