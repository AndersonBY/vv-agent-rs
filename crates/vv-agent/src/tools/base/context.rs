use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use super::paths::resolve_workspace_path_checked;
use super::SubTaskRunner;
use crate::model::ModelProvider;
use crate::types::{ToolArguments, ToolCall};
use crate::workspace::{
    DiscoveryFilteredWorkspaceBackend, LocalWorkspaceBackend, WorkspaceBackend,
};
use crate::{RunConfig, RunContext};

#[derive(Clone)]
pub struct ToolContext {
    pub workspace: PathBuf,
    pub shared_state: BTreeMap<String, Value>,
    pub cycle_index: u32,
    pub task_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: ToolArguments,
    pub metadata: BTreeMap<String, Value>,
    pub app_state: Option<Arc<dyn std::any::Any + Send + Sync>>,
    pub workspace_backend: Arc<dyn WorkspaceBackend>,
    pub model_provider: Option<Arc<dyn ModelProvider>>,
    pub run_context: Option<RunContext>,
    pub sub_task_runner: Option<SubTaskRunner>,
    pub sub_task_manager: Option<crate::runtime::sub_task_manager::SubTaskManager>,
    pub sub_task_turn_snapshot: Option<crate::runtime::sub_task_manager::SubTaskTurnSnapshot>,
    pub execution_backend: Option<crate::runtime::backends::RuntimeExecutionBackend>,
    #[doc(hidden)]
    pub background_parent_run_config: Option<RunConfig>,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolContext")
            .field("workspace", &self.workspace)
            .field("shared_state", &self.shared_state)
            .field("cycle_index", &self.cycle_index)
            .field("task_id", &self.task_id)
            .field("tool_call_id", &self.tool_call_id)
            .field("tool_name", &self.tool_name)
            .field("arguments", &self.arguments)
            .field("metadata", &self.metadata)
            .field("has_app_state", &self.app_state.is_some())
            .field("has_model_provider", &self.model_provider.is_some())
            .field("has_run_context", &self.run_context.is_some())
            .field("has_sub_task_runner", &self.sub_task_runner.is_some())
            .field("has_sub_task_manager", &self.sub_task_manager.is_some())
            .field(
                "has_sub_task_turn_snapshot",
                &self.sub_task_turn_snapshot.is_some(),
            )
            .field("has_execution_backend", &self.execution_backend.is_some())
            .field(
                "has_background_parent_run_config",
                &self.background_parent_run_config.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl ToolContext {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        let workspace = workspace.into();
        Self {
            workspace: workspace.clone(),
            shared_state: BTreeMap::new(),
            cycle_index: 0,
            task_id: String::new(),
            tool_call_id: String::new(),
            tool_name: String::new(),
            arguments: ToolArguments::new(),
            metadata: BTreeMap::new(),
            app_state: None,
            workspace_backend: Arc::new(crate::workspace::LocalWorkspaceBackend::new(workspace)),
            model_provider: None,
            run_context: None,
            sub_task_runner: None,
            sub_task_manager: None,
            sub_task_turn_snapshot: None,
            execution_backend: None,
            background_parent_run_config: None,
        }
    }

    pub fn allow_outside_workspace_paths(&self) -> bool {
        for key in [
            "allow_outside_workspace_paths",
            "allow_outside_workspace",
            "workspace_allow_outside_main",
            "workspace_allow_outside",
        ] {
            if let Some(parsed) = parse_bool(self.metadata.get(key)) {
                return parsed;
            }
        }
        false
    }

    pub fn begin_tool_call(&mut self, call: &ToolCall) {
        self.tool_call_id = call.id.clone();
        self.tool_name = call.name.clone();
        self.arguments = call.arguments.clone();
        if let Some(snapshot) = self.sub_task_turn_snapshot.as_mut() {
            snapshot.parent_tool_call_id = (!call.id.trim().is_empty()).then(|| call.id.clone());
        }
    }

    pub fn app_state<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.app_state.as_ref()?.downcast_ref::<T>()
    }

    pub fn resolve_workspace_path(&self, raw_path: &str) -> Result<PathBuf, String> {
        resolve_workspace_path_checked(
            &self.workspace,
            raw_path,
            self.allow_outside_workspace_paths(),
        )
    }

    pub fn effective_workspace_backend(&self) -> Arc<dyn WorkspaceBackend> {
        if self.allow_outside_workspace_paths() {
            if let Some(filtered) = self
                .workspace_backend
                .as_any()
                .downcast_ref::<DiscoveryFilteredWorkspaceBackend>()
            {
                if let Some(local) = filtered
                    .inner()
                    .as_any()
                    .downcast_ref::<LocalWorkspaceBackend>()
                {
                    let mut local = local.clone();
                    local.allow_outside_root = true;
                    return Arc::new(
                        DiscoveryFilteredWorkspaceBackend::new(Arc::new(local), filtered.pattern())
                            .expect("existing discovery filter remains valid"),
                    );
                }
            }
            if let Some(local) = self
                .workspace_backend
                .as_any()
                .downcast_ref::<LocalWorkspaceBackend>()
            {
                let mut local = local.clone();
                local.allow_outside_root = true;
                return Arc::new(local);
            }
        }
        self.workspace_backend.clone()
    }
}

fn parse_bool(value: Option<&Value>) -> Option<bool> {
    match value {
        Some(Value::Bool(value)) => Some(*value),
        Some(Value::Number(number)) => number.as_i64().and_then(|value| match value {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }),
        Some(Value::String(value)) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
}
