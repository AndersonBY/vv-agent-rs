use std::path::PathBuf;
use std::sync::Arc;

use crate::model::ModelRef;
use crate::types::Metadata;

#[derive(Clone, Default)]
pub struct RunContext {
    pub run_id: String,
    pub agent_name: String,
    pub model: Option<ModelRef>,
    pub workspace: Option<PathBuf>,
    pub metadata: Metadata,
}

#[derive(Clone)]
pub struct ToolCallContext {
    pub run: RunContext,
    pub tool_call_id: String,
    pub tool_name: String,
    pub raw_arguments: serde_json::Value,
    pub metadata: Metadata,
    pub app_state: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

impl ToolCallContext {
    pub fn app_state<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.app_state.as_ref()?.downcast_ref::<T>()
    }
}
