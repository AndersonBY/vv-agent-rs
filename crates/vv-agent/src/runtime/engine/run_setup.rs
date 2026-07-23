use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::llm::{LlmClient, LlmError};
use crate::memory::MemoryManager;
use crate::model::{ModelProvider, VvLlmModelProvider};
use crate::runtime::model_calls::{ModelBudgetObserver, ModelCallCoordinator, ModelCallLedger};
use crate::runtime::sub_task_manager::SubTaskManager;
use crate::types::{AgentTask, CycleRecord, Message, Metadata};
use crate::workspace::{LocalWorkspaceBackend, WorkspaceBackend};

use super::budget::{budget_snapshot, lock_budget, SharedRunBudgetController};
use super::checkpoint::CheckpointCoordinator;
use super::helpers::{build_initial_messages, seed_skill_state_from_task_metadata};
use super::memory::{build_memory_manager, build_runtime_memory_callbacks};
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
    pub memory_model_provider: Option<Arc<dyn ModelProvider>>,
}

pub(super) struct PreparedRuntimeAccounting {
    pub model_call_ledger: ModelCallLedger,
    pub model_call_coordinator: ModelCallCoordinator,
    pub checkpoint: CheckpointCoordinator,
    pub memory_manager: MemoryManager,
}

pub(super) fn prepare_approval_broker(controls: &mut RuntimeRunControls) {
    if let Some(context) = controls.execution_context.as_mut() {
        if context.approval_provider.is_some() && context.approval_broker.is_none() {
            context.approval_broker = Some(crate::approval::ApprovalBroker::default());
        }
    }
}

pub(super) fn prepare_run_setup<C>(
    runtime: &AgentRuntime<C>,
    task: AgentTask,
    controls: &RuntimeRunControls,
) -> Result<PreparedRun, crate::llm::LlmError>
where
    C: LlmClient + Clone + 'static,
{
    let mut task = task;
    let messages = controls
        .initial_messages
        .clone()
        .unwrap_or_else(|| build_initial_messages(&task));
    crate::runtime::tool_planner::freeze_dynamic_tool_schema_hints(&mut task);

    let cycles = controls.initial_cycles.clone().unwrap_or_default();
    let mut shared_state = controls
        .initial_shared_state
        .clone()
        .unwrap_or_else(|| task.initial_shared_state.clone());
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
    let memory_model_provider = controls.model_provider.clone().or_else(|| {
        let settings_file = runtime.settings_file.as_ref()?.clone();
        if !settings_file.is_file() {
            return None;
        }
        let mut provider = VvLlmModelProvider::from_settings_file(settings_file)
            .with_timeout_seconds(runtime.sub_agent_timeout_seconds);
        if let Some(default_backend) = runtime.default_backend.as_ref() {
            provider = provider.with_default_backend(default_backend.clone());
        }
        Some(Arc::new(provider) as Arc<dyn ModelProvider>)
    });
    let memory_manager = build_memory_manager(
        &task,
        workspace_path.clone(),
        runtime.settings_file.as_deref(),
        runtime.default_backend.as_deref(),
    )
    .map_err(crate::llm::LlmError::Request)?;

    Ok(PreparedRun {
        task,
        messages,
        cycles,
        shared_state,
        workspace_path,
        workspace_backend,
        sub_task_manager,
        memory_manager,
        memory_model_provider,
    })
}

pub(super) fn prepare_runtime_accounting<C>(
    runtime: &AgentRuntime<C>,
    task: &AgentTask,
    controls: &mut RuntimeRunControls,
    mut memory_manager: MemoryManager,
    memory_model_provider: Option<Arc<dyn ModelProvider>>,
    budget_controller: &Option<SharedRunBudgetController>,
) -> Result<PreparedRuntimeAccounting, LlmError>
where
    C: LlmClient + Clone + 'static,
{
    let model_call_ledger = controls
        .execution_context
        .as_ref()
        .map(|context| context.runtime_state.model_call_ledger.clone())
        .unwrap_or_default();
    if let Some(initial_model_calls) = controls.initial_model_calls.clone() {
        model_call_ledger
            .replace(initial_model_calls)
            .map_err(LlmError::Request)?;
    }

    let (run_id, trace_id, agent_name, session_id, parent_run_id, backend, model) = {
        let runtime_metadata = controls
            .execution_context
            .as_ref()
            .map(|context| &context.metadata)
            .unwrap_or(&task.metadata);
        let identity = |keys: &[&str]| {
            keys.iter().find_map(|key| {
                runtime_metadata
                    .get(*key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
        };
        let run_id =
            identity(&["_vv_agent_run_id", "run_id"]).unwrap_or_else(|| task.task_id.clone());
        let trace_id =
            identity(&["_vv_agent_trace_id", "trace_id"]).unwrap_or_else(|| run_id.clone());
        let agent_name = identity(&["_vv_agent_agent_name", "agent_name"])
            .unwrap_or_else(|| task.task_id.clone());
        let session_id = identity(&["_vv_agent_session_id", "session_id"]);
        let parent_run_id = identity(&["_vv_agent_parent_run_id", "parent_run_id"]);
        let backend = identity(&["_vv_agent_resolved_backend"])
            .or_else(|| runtime.default_backend.clone())
            .unwrap_or_else(|| "direct".to_string());
        let model = identity(&["_vv_agent_resolved_model"]).unwrap_or_else(|| task.model.clone());
        (
            run_id,
            trace_id,
            agent_name,
            session_id,
            parent_run_id,
            backend,
            model,
        )
    };

    let event_handler = controls
        .effective_event_handler()
        .or_else(|| runtime.event_handler.clone());
    let checkpoint = CheckpointCoordinator::new(
        controls.effective_checkpoint_controller().cloned(),
        model_call_ledger.clone(),
    );
    let cancellation_token = controls.effective_cancellation_token();
    let budget_observer: Option<ModelBudgetObserver> = budget_controller.as_ref().map(|value| {
        let controller = Arc::clone(value);
        let cancellation_token = cancellation_token.clone();
        Arc::new(move |cycle_index: u32, usage: &crate::types::TokenUsage| {
            let suppress_exhaustion = cancellation_token
                .as_ref()
                .is_some_and(crate::runtime::cancellation::CancellationToken::is_cancelled);
            lock_budget(&controller).model_call_complete(cycle_index, usage, suppress_exhaustion)
        }) as ModelBudgetObserver
    });
    let model_call_coordinator = ModelCallCoordinator::new(
        model_call_ledger.clone(),
        run_id.clone(),
        trace_id.clone(),
        agent_name.clone(),
        session_id.clone(),
        parent_run_id.clone(),
        event_handler.clone(),
        budget_observer,
    );
    if let Some(context) = controls.execution_context.as_mut() {
        context.runtime_state.model_call_ledger = model_call_ledger.clone();
        context.runtime_state.model_call_coordinator = Some(model_call_coordinator.clone());
    }
    checkpoint.bind_model_accounting(&model_call_coordinator)?;

    let memory_budget_controller = budget_controller.clone();
    let memory_budget_snapshot = Arc::new(move || budget_snapshot(&memory_budget_controller));
    let memory_budget_controller = budget_controller.clone();
    let memory_budget_exhaustion = Arc::new(move || {
        memory_budget_controller
            .as_ref()
            .and_then(|controller| lock_budget(controller).exhaustion().cloned())
    });
    memory_manager = memory_manager.with_runtime_callbacks(build_runtime_memory_callbacks(
        memory_model_provider,
        Arc::new(runtime.llm_client.clone()),
        backend,
        model,
        checkpoint.clone(),
        model_call_coordinator.clone(),
        memory_budget_snapshot,
        memory_budget_exhaustion,
        cancellation_token,
        run_id,
        trace_id,
        agent_name,
        session_id,
        parent_run_id,
        event_handler.clone(),
    ));

    Ok(PreparedRuntimeAccounting {
        model_call_ledger,
        model_call_coordinator,
        checkpoint,
        memory_manager,
    })
}
