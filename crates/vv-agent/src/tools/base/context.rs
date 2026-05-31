use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use super::paths::resolve_workspace_path_checked;
use super::SubTaskRunner;
use crate::model::ModelProvider;
use crate::workspace::{LocalWorkspaceBackend, WorkspaceBackend};

#[derive(Clone)]
pub struct ToolContext {
    pub workspace: PathBuf,
    pub shared_state: BTreeMap<String, Value>,
    pub cycle_index: u32,
    pub task_id: String,
    pub metadata: BTreeMap<String, Value>,
    pub workspace_backend: Arc<dyn WorkspaceBackend>,
    pub model_provider: Option<Arc<dyn ModelProvider>>,
    pub sub_task_runner: Option<SubTaskRunner>,
    pub sub_task_manager: Option<crate::runtime::sub_task_manager::SubTaskManager>,
    pub execution_backend: Option<crate::runtime::backends::RuntimeExecutionBackend>,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolContext")
            .field("workspace", &self.workspace)
            .field("shared_state", &self.shared_state)
            .field("cycle_index", &self.cycle_index)
            .field("task_id", &self.task_id)
            .field("metadata", &self.metadata)
            .field("has_model_provider", &self.model_provider.is_some())
            .field("has_sub_task_runner", &self.sub_task_runner.is_some())
            .field("has_sub_task_manager", &self.sub_task_manager.is_some())
            .field("has_execution_backend", &self.execution_backend.is_some())
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
            metadata: BTreeMap::new(),
            workspace_backend: Arc::new(crate::workspace::LocalWorkspaceBackend::new(workspace)),
            model_provider: None,
            sub_task_runner: None,
            sub_task_manager: None,
            execution_backend: None,
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

    pub fn resolve_workspace_path(&self, raw_path: &str) -> Result<PathBuf, String> {
        resolve_workspace_path_checked(
            &self.workspace,
            raw_path,
            self.allow_outside_workspace_paths(),
        )
    }

    pub fn effective_workspace_backend(&self) -> Arc<dyn WorkspaceBackend> {
        if self.allow_outside_workspace_paths() {
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
