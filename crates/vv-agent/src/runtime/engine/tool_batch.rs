use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::budget::RunBudgetLimits;
use crate::llm::LlmClient;
use crate::runtime::cancellation::CancellationToken;
use crate::runtime::sub_agents::SubTaskRunControls;
use crate::runtime::sub_task_manager::{SubTaskManager, SubTaskTurnSnapshot};
use crate::tools::{
    ToolContext, ToolLifecycleEvent, ToolOrchestrator, ToolRunOptions, ToolSpecKind,
};
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
                event_handler: controls
                    .effective_event_handler()
                    .or_else(|| self.event_handler.clone()),
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
                event_handler: controls
                    .effective_event_handler()
                    .or_else(|| self.event_handler.clone()),
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
        let runtime_handler = self.event_handler.clone();
        let event_handler = controls.effective_event_handler();
        let execution_context = controls.execution_context.clone();
        let task_id = task.task_id.clone();
        let agent_name = task
            .metadata
            .get("agent_name")
            .and_then(Value::as_str)
            .unwrap_or(&task.task_id)
            .to_string();
        let sub_run_tool_names = request_tool_schemas
            .iter()
            .filter_map(|schema| schema["function"]["name"].as_str())
            .filter(|name| {
                self.tool_registry.get(name).is_ok_and(|spec| {
                    matches!(
                        spec.kind,
                        ToolSpecKind::Agent | ToolSpecKind::BackgroundAgent
                    )
                })
            })
            .map(str::to_string)
            .collect::<std::collections::BTreeSet<_>>();
        options = options.lifecycle_callback(Arc::new(move |event| {
            let sub_run_call_id = match &event {
                ToolLifecycleEvent::Started { call, .. }
                    if sub_run_tool_names.contains(&call.name) =>
                {
                    Some(call.id.clone())
                }
                _ => None,
            };
            let (event_name, mut payload) = lifecycle_log_payload(event, cycle_index);
            payload.insert("task_id".to_string(), Value::String(task_id.clone()));
            payload.insert("agent_name".to_string(), Value::String(agent_name.clone()));
            super::logging::emit_runtime_event(
                runtime_handler.as_ref(),
                event_handler.as_ref(),
                execution_context.as_ref(),
                event_name,
                payload,
            );
            if let Some(tool_call_id) = sub_run_call_id {
                super::logging::emit_runtime_event(
                    runtime_handler.as_ref(),
                    event_handler.as_ref(),
                    execution_context.as_ref(),
                    "sub_run_started",
                    BTreeMap::from([
                        ("task_id".to_string(), Value::String(task_id.clone())),
                        ("agent_name".to_string(), Value::String(agent_name.clone())),
                        ("cycle".to_string(), Value::from(cycle_index)),
                        ("parent_run_id".to_string(), Value::String(task_id.clone())),
                        (
                            "parent_tool_call_id".to_string(),
                            Value::String(tool_call_id.clone()),
                        ),
                        (
                            "task_id_hint".to_string(),
                            Value::String(format!("sub_run:{tool_call_id}")),
                        ),
                    ]),
                );
            }
        }));
        PreparedToolBatch {
            context,
            orchestrator,
            options,
        }
    }
}

fn lifecycle_log_payload(
    event: ToolLifecycleEvent,
    cycle_index: u32,
) -> (&'static str, BTreeMap<String, Value>) {
    let mut payload = BTreeMap::from([("cycle".to_string(), Value::from(cycle_index))]);
    let planned = matches!(&event, ToolLifecycleEvent::Planned { .. });
    match event {
        ToolLifecycleEvent::Planned {
            call,
            tool_metadata,
        }
        | ToolLifecycleEvent::Started {
            call,
            tool_metadata,
        } => {
            let event_name = if planned {
                "tool_call_planned"
            } else {
                "tool_call_started"
            };
            payload.insert("tool_name".to_string(), Value::String(call.name));
            payload.insert("tool_call_id".to_string(), Value::String(call.id));
            payload.insert(
                "arguments".to_string(),
                Value::Object(call.arguments.into_iter().collect()),
            );
            if let Some(tool_metadata) = tool_metadata {
                payload.insert(
                    "tool_metadata".to_string(),
                    serde_json::to_value(tool_metadata)
                        .expect("normalized tool metadata must serialize"),
                );
            }
            (event_name, payload)
        }
        ToolLifecycleEvent::Completed {
            call,
            result,
            execution_started,
            duration_ms,
            tool_metadata,
        } => {
            payload.insert("tool_name".to_string(), Value::String(call.name));
            payload.insert(
                "tool_call_id".to_string(),
                Value::String(result.tool_call_id),
            );
            payload.insert(
                "status".to_string(),
                super::logging::tool_result_status_value(result.status),
            );
            payload.insert(
                "directive".to_string(),
                serde_json::to_value(result.directive).expect("tool directive must serialize"),
            );
            payload.insert(
                "error_code".to_string(),
                result.error_code.map(Value::String).unwrap_or(Value::Null),
            );
            payload.insert(
                "execution_started".to_string(),
                Value::Bool(execution_started),
            );
            payload.insert(
                "duration_ms".to_string(),
                duration_ms.map(Value::from).unwrap_or(Value::Null),
            );
            if let Some(tool_metadata) = tool_metadata {
                payload.insert(
                    "tool_metadata".to_string(),
                    serde_json::to_value(tool_metadata)
                        .expect("normalized tool metadata must serialize"),
                );
            }
            ("tool_call_completed", payload)
        }
    }
}
