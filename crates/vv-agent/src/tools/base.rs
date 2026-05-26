use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::types::{ToolArguments, ToolExecutionResult};
use crate::workspace::{LocalWorkspaceBackend, WorkspaceBackend};

pub type ToolHandler =
    Arc<dyn Fn(&mut ToolContext, &ToolArguments) -> ToolExecutionResult + Send + Sync + 'static>;
pub type SubTaskRunner = Arc<
    dyn Fn(crate::types::SubTaskRequest) -> crate::types::SubTaskOutcome + Send + Sync + 'static,
>;

#[derive(Clone)]
pub struct ToolContext {
    pub workspace: PathBuf,
    pub shared_state: BTreeMap<String, Value>,
    pub cycle_index: u32,
    pub task_id: String,
    pub metadata: BTreeMap<String, Value>,
    pub workspace_backend: Arc<dyn WorkspaceBackend>,
    pub sub_task_runner: Option<SubTaskRunner>,
    pub sub_task_manager: Option<crate::sub_task_manager::SubTaskManager>,
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
            .field("has_sub_task_runner", &self.sub_task_runner.is_some())
            .field("has_sub_task_manager", &self.sub_task_manager.is_some())
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
            sub_task_runner: None,
            sub_task_manager: None,
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

pub(crate) fn resolve_workspace_path_checked(
    workspace: &Path,
    raw_path: &str,
    allow_outside_workspace_paths: bool,
) -> Result<PathBuf, String> {
    let base = workspace
        .canonicalize()
        .unwrap_or_else(|_| absolutize_without_canonicalizing(workspace));
    let candidate = Path::new(raw_path);
    let target = if candidate.is_absolute() {
        absolutize_without_canonicalizing(candidate)
    } else {
        absolutize_without_canonicalizing(&base.join(candidate))
    };
    let normalized = normalize_path(target);
    if !allow_outside_workspace_paths && normalized != base && !normalized.starts_with(&base) {
        return Err(format!("Path escapes workspace: {raw_path}"));
    }
    Ok(normalized)
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

fn absolutize_without_canonicalizing(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn normalize_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

#[derive(Clone)]
pub struct ToolSpec {
    pub name: String,
    pub handler: ToolHandler,
    pub description: String,
    pub schema: Value,
}

impl ToolSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        handler: ToolHandler,
    ) -> Self {
        let name = name.into();
        let description = description.into();
        let schema = super::schemas::schema_for(&name).unwrap_or_else(|| {
            json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": {"type": "object", "properties": {}, "required": []},
                }
            })
        });
        Self {
            schema,
            name,
            handler,
            description,
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("tool not found: {0}")]
pub struct ToolNotFoundError(pub String);
