use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use super::state::StateStore;
use super::CancellationToken;

pub type StreamCallback = Arc<dyn Fn(&BTreeMap<String, Value>) + Send + Sync + 'static>;

#[derive(Clone, Default)]
pub struct ExecutionContext {
    pub cancellation_token: Option<CancellationToken>,
    pub stream_callback: Option<StreamCallback>,
    pub state_store: Option<Arc<dyn StateStore>>,
    pub metadata: BTreeMap<String, Value>,
}

impl std::fmt::Debug for ExecutionContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionContext")
            .field("has_cancellation_token", &self.cancellation_token.is_some())
            .field("has_stream_callback", &self.stream_callback.is_some())
            .field("has_state_store", &self.state_store.is_some())
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
