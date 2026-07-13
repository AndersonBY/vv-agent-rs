use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::model::ModelRef;
use crate::types::Metadata;

#[derive(Clone, Default)]
pub struct RunContext {
    pub run_id: String,
    pub agent_name: String,
    pub model: Option<ModelRef>,
    pub workspace: Option<PathBuf>,
    pub metadata: Metadata,
    pub app_state: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

impl RunContext {
    pub fn app_state<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.app_state.as_ref()?.downcast_ref::<T>()
    }
}

#[derive(Clone)]
pub struct ToolCallContext {
    pub run: RunContext,
    pub tool_call_id: String,
    pub tool_name: String,
    pub raw_arguments: serde_json::Value,
    pub metadata: Metadata,
    pub app_state: Option<Arc<dyn std::any::Any + Send + Sync>>,
    pub shared_state: Arc<Mutex<Metadata>>,
}

impl ToolCallContext {
    pub fn app_state<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.app_state.as_ref()?.downcast_ref::<T>()
    }

    pub fn shared_state_snapshot(&self) -> Metadata {
        self.shared_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn shared_state_value(&self, key: &str) -> Option<serde_json::Value> {
        self.shared_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .cloned()
    }

    pub fn set_shared_state_value(
        &self,
        key: impl Into<String>,
        value: serde_json::Value,
    ) -> Option<serde_json::Value> {
        self.shared_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.into(), value)
    }

    pub fn remove_shared_state_value(&self, key: &str) -> Option<serde_json::Value> {
        self.shared_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(key)
    }
}
