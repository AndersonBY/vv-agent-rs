use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;
use thiserror::Error;

use crate::memory::CompactionExhaustedError;
use crate::types::{LLMResponse, Message};

pub type LlmStreamCallback = Arc<dyn Fn(&BTreeMap<String, Value>) + Send + Sync + 'static>;

#[derive(Debug, Clone, PartialEq)]
pub struct LlmRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<Value>,
    pub metadata: Value,
}

impl LlmRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            tools: Vec::new(),
            metadata: Value::Null,
        }
    }
}

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("scripted response queue is empty")]
    ScriptExhausted,
    #[error("{0}")]
    CompactionExhausted(CompactionExhaustedError),
    #[error("llm request failed: {0}")]
    Request(String),
}

pub trait LlmClient: Send + Sync {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError>;

    fn set_debug_dump_dir(&self, _debug_dump_dir: &Path) {}

    fn clone_with_debug_dump_dir(&self, _debug_dump_dir: &Path) -> Option<Arc<dyn LlmClient>> {
        None
    }

    fn complete_with_stream(
        &self,
        request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        let _ = stream_callback;
        self.complete(request)
    }
}

impl<T> LlmClient for Arc<T>
where
    T: LlmClient + ?Sized,
{
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        (**self).complete(request)
    }

    fn set_debug_dump_dir(&self, debug_dump_dir: &Path) {
        (**self).set_debug_dump_dir(debug_dump_dir);
    }

    fn clone_with_debug_dump_dir(&self, debug_dump_dir: &Path) -> Option<Arc<dyn LlmClient>> {
        (**self).clone_with_debug_dump_dir(debug_dump_dir)
    }

    fn complete_with_stream(
        &self,
        request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        (**self).complete_with_stream(request, stream_callback)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointTarget {
    pub endpoint_id: String,
    pub api_key: String,
    pub api_base: String,
    pub endpoint_type: String,
    pub model_id: String,
}

impl EndpointTarget {
    pub fn new(
        endpoint_id: impl Into<String>,
        api_key: impl Into<String>,
        api_base: impl Into<String>,
        endpoint_type: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Self {
        Self {
            endpoint_id: endpoint_id.into(),
            api_key: api_key.into(),
            api_base: api_base.into(),
            endpoint_type: endpoint_type.into(),
            model_id: model_id.into(),
        }
    }
}
