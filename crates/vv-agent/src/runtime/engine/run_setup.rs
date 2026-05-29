use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::llm::LlmClient;
use crate::memory::MemoryManager;
use crate::runtime::sub_task_manager::SubTaskManager;
use crate::types::{AgentTask, CycleRecord, Message, Metadata};
use crate::workspace::{LocalWorkspaceBackend, WorkspaceBackend};

use super::helpers::{build_initial_messages, seed_skill_state_from_task_metadata};
use super::memory::build_memory_manager;
use super::{AgentRuntime, RuntimeRunControls};

pub(super) struct PreparedRun {
    pub task: AgentTask,
    pub messages: Vec<Message>,
    pub cycles: Vec<CycleRecord>,
    pub shared_state: Metadata,
    pub workspace_path: PathBuf,
    pub workspace_backend: Arc<dyn WorkspaceBackend>,
    pub sub_task_manager: SubTaskManager,
    pub memory_manager: MemoryManager,
}

pub(super) fn prepare_run_setup<C>(
    runtime: &AgentRuntime<C>,
    task: AgentTask,
    controls: &RuntimeRunControls,
) -> PreparedRun
where
    C: LlmClient + Clone + 'static,
{
    let mut task = task;
    let messages = build_initial_messages(&task);
    crate::runtime::tool_planner::freeze_dynamic_tool_schema_hints(&mut task);

    let cycles = Vec::new();
    let mut shared_state = task.initial_shared_state.clone();
    shared_state
        .entry("todo_list".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    seed_skill_state_from_task_metadata(&mut shared_state, &task.metadata);

    let workspace_path = runtime
        .default_workspace
        .clone()
        .unwrap_or_else(|| PathBuf::from("./workspace"));
    let workspace_path = controls.workspace.clone().unwrap_or(workspace_path);
    let workspace_backend = controls.workspace_backend.clone().unwrap_or_else(|| {
        if controls.workspace.is_some() {
            Arc::new(LocalWorkspaceBackend::new(workspace_path.clone()))
        } else {
            runtime.workspace_backend.clone()
        }
    });
    let sub_task_manager = controls.sub_task_manager.clone().unwrap_or_default();
    let memory_manager = build_memory_manager(
        &task,
        workspace_path.clone(),
        Some(runtime.llm_client.clone()),
        runtime.settings_file.as_deref(),
        runtime.default_backend.as_deref(),
    );

    PreparedRun {
        task,
        messages,
        cycles,
        shared_state,
        workspace_path,
        workspace_backend,
        sub_task_manager,
        memory_manager,
    }
}
