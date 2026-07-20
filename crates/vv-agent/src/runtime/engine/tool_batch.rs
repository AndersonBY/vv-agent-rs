use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::budget::RunBudgetLimits;
use crate::llm::{LlmClient, LlmStreamCallback};
use crate::runtime::cancellation::CancellationToken;
use crate::runtime::sub_agents::SubTaskRunControls;
use crate::runtime::sub_task_manager::{SubTaskManager, SubTaskTurnSnapshot};
use crate::tools::{ToolContext, ToolOrchestrator, ToolRunOptions};
use crate::types::AgentTask;
use crate::workspace::WorkspaceBackend;

use super::{AgentRuntime, RuntimeRunControls};

pub(super) struct ToolBatchSetup<'a> {
    pub(super) task: &'a AgentTask,
    pub(super) controls: &'a RuntimeRunControls,
    pub(super) workspace_path: &'a PathBuf,
    pub(super) workspace_backend: &'a Arc<dyn WorkspaceBackend>,
    pub(super) shared_state: &'a BTreeMap<String, Value>,
    pub(super) sub_task_manager: &'a SubTaskManager,
    pub(super) cycle_index: u32,
    pub(super) cancellation_token: Option<&'a CancellationToken>,
    pub(super) stream_callback: &'a Option<LlmStreamCallback>,
    pub(super) child_budget_limits: &'a Option<RunBudgetLimits>,
    pub(super) request_tool_schemas: &'a [Value],
    pub(super) after_cycle_disallowed_tools: &'a [String],
}

pub(super) struct PreparedToolBatch {
    pub(super) context: ToolContext,
    pub(super) orchestrator: ToolOrchestrator,
    pub(super) options: ToolRunOptions,
}

impl<C: LlmClient + Clone + 'static> AgentRuntime<C> {
    pub(super) fn prepare_tool_batch(&self, setup: ToolBatchSetup<'_>) -> PreparedToolBatch {
        let ToolBatchSetup {
            task,
            controls,
            workspace_path,
            workspace_backend,
            shared_state,
            sub_task_manager,
            cycle_index,
            cancellation_token,
            stream_callback,
            child_budget_limits,
            request_tool_schemas,
            after_cycle_disallowed_tools,
        } = setup;
        let sub_task_runner = self.build_sub_task_runner(
            task,
            workspace_path.clone(),
            workspace_backend.clone(),
            shared_state.clone(),
            sub_task_manager.clone(),
            SubTaskRunControls {
                parent_cancellation_token: cancellation_token.cloned(),
                stream_callback: stream_callback.clone(),
                parent_log_handler: self.log_handler.clone(),
                parent_event_handler: controls.log_handler.clone(),
                parent_execution_context: controls.execution_context.clone(),
                model_provider: controls.model_provider.clone(),
                parent_run_context: controls.run_context.clone(),
                tool_policy: self.tool_policy.clone(),
                budget_limits: child_budget_limits.clone(),
            },
        );
        let mut tool_metadata = task.metadata.clone();
        for key in [
            "_vv_agent_agent_name",
            "_vv_agent_parent_run_id",
            "_vv_agent_parent_tool_call_id",
            "_vv_agent_run_id",
            "_vv_agent_session_id",
            "_vv_agent_trace_id",
        ] {
            tool_metadata.remove(key);
        }
        if let Some(execution_context) = controls.execution_context.as_ref() {
            tool_metadata.extend(execution_context.metadata.clone());
        }
        let trace_id = controls
            .execution_context
            .as_ref()
            .and_then(|context| {
                ["_vv_agent_trace_id", "trace_id"]
                    .into_iter()
                    .find_map(|key| {
                        context
                            .metadata
                            .get(key)
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string)
                    })
            })
            .or_else(|| {
                controls.run_context.as_ref().and_then(|run| {
                    ["_vv_agent_trace_id", "trace_id"]
                        .into_iter()
                        .find_map(|key| {
                            run.metadata
                                .get(key)
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|value| !value.is_empty())
                                .map(str::to_string)
                        })
                })
            })
            .or_else(|| {
                ["_vv_agent_trace_id", "trace_id"]
                    .into_iter()
                    .find_map(|key| {
                        task.metadata
                            .get(key)
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string)
                    })
            });
        let parent_run_id = controls
            .run_context
            .as_ref()
            .map(|run| run.run_id.trim())
            .filter(|run_id| !run_id.is_empty())
            .map(str::to_string)
            .or_else(|| {
                controls.execution_context.as_ref().and_then(|context| {
                    context
                        .metadata
                        .get("_vv_agent_run_id")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                })
            });
        let context = ToolContext {
            workspace: workspace_path.clone(),
            shared_state: shared_state.clone(),
            cycle_index,
            task_id: task.task_id.clone(),
            tool_call_id: String::new(),
            tool_name: String::new(),
            arguments: crate::types::ToolArguments::new(),
            idempotency_key: None,
            metadata: tool_metadata,
            app_state: controls
                .execution_context
                .as_ref()
                .and_then(|context| context.app_state.clone()),
            workspace_backend: workspace_backend.clone(),
            model_provider: controls.model_provider.clone(),
            run_context: controls.run_context.clone(),
            sub_task_runner,
            sub_task_manager: Some(sub_task_manager.clone()),
            sub_task_turn_snapshot: Some(SubTaskTurnSnapshot {
                cancellation_token: cancellation_token.cloned(),
                event_handler: controls.log_handler.clone(),
                stream_callback: stream_callback.clone(),
                trace_id,
                parent_run_id,
                parent_tool_call_id: None,
                parent_execution_context: controls.execution_context.clone(),
                parent_run_context: controls.run_context.clone(),
                tool_policy: self.tool_policy.clone().unwrap_or_default(),
            }),
            execution_backend: Some(self.execution_backend.clone()),
            background_parent_run_config: controls.background_parent_run_config.clone(),
        };
        let orchestrator = ToolOrchestrator::from_tools(self.tool_registry.executors());
        let planned_tool_names = request_tool_schemas
            .iter()
            .filter_map(|schema| schema["function"]["name"].as_str().map(str::to_string))
            .collect::<Vec<_>>();
        let mut options = self
            .tool_policy
            .as_ref()
            .map(ToolRunOptions::from_policy)
            .unwrap_or_default()
            .planned_names(planned_tool_names);
        for tool_name in after_cycle_disallowed_tools {
            options = options.disallow(tool_name.clone());
        }
        PreparedToolBatch {
            context,
            orchestrator,
            options,
        }
    }
}
