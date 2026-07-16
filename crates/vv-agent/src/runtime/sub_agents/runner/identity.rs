use serde_json::Value;

use crate::runtime::sub_task_manager::SubTaskManager;
use crate::types::{Metadata, SubTaskRequest};

use super::super::invocation::take_assigned_sub_task_identity;
use super::super::types::{SubRunLifecycle, SubTaskRunContext};

pub(super) fn resolve_sub_task_identity(
    context: &SubTaskRunContext,
    request: &SubTaskRequest,
) -> SubRunLifecycle {
    let parent_task = &context.parent_task;
    let (task_id, session_id) = take_assigned_sub_task_identity()
        .map(|identity| (identity.task_id, identity.session_id))
        .unwrap_or_else(|| {
            SubTaskManager::next_task_identity(&parent_task.task_id, &request.agent_name)
        });

    let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());
    let parent_run_id = context
        .parent_run_context
        .as_ref()
        .map(|run| run.run_id.trim())
        .filter(|run_id| !run_id.is_empty())
        .map(str::to_string)
        .or_else(|| {
            context
                .parent_execution_context
                .as_ref()
                .and_then(|execution| {
                    execution
                        .metadata
                        .get("_vv_agent_run_id")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|run_id| !run_id.is_empty())
                        .map(str::to_string)
                })
        })
        .or_else(|| metadata_string(request, "parent_run_id"))
        .unwrap_or_default();
    let trace_id = context
        .parent_execution_context
        .as_ref()
        .and_then(|execution| {
            metadata_value(&execution.metadata, &["_vv_agent_trace_id", "trace_id"])
        })
        .or_else(|| {
            context
                .parent_run_context
                .as_ref()
                .and_then(|run| metadata_value(&run.metadata, &["trace_id"]))
        })
        .or_else(|| metadata_value(&parent_task.metadata, &["trace_id"]))
        .unwrap_or_else(|| run_id.clone());

    SubRunLifecycle {
        run_id,
        trace_id,
        parent_run_id,
        parent_tool_call_id: metadata_string(request, "parent_tool_call_id").unwrap_or_default(),
        task_id,
        session_id,
        agent_name: request.agent_name.clone(),
        parent_task_id: parent_task.task_id.clone(),
        model: String::new(),
    }
}

fn metadata_string(request: &SubTaskRequest, key: &str) -> Option<String> {
    metadata_value(&request.metadata, &[key])
}

fn metadata_value(metadata: &Metadata, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        metadata
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::resolve_sub_task_identity;
    use crate::llm::ScriptedLlmClient;
    use crate::runtime::sub_agents::types::SubTaskRunContext;
    use crate::runtime::{ExecutionContext, SubTaskManager};
    use crate::tools::build_default_registry;
    use crate::types::{AgentTask, SubTaskRequest};
    use crate::workspace::MemoryWorkspaceBackend;
    use crate::RunContext;

    fn context() -> SubTaskRunContext {
        SubTaskRunContext {
            llm_client: Arc::new(ScriptedLlmClient::new(Vec::new())),
            tool_registry: build_default_registry(),
            workspace_backend: Arc::new(MemoryWorkspaceBackend::default()),
            workspace_path: std::path::PathBuf::from("."),
            parent_task: AgentTask::new("parent-task", "parent-model", "Parent prompt", "Delegate"),
            parent_shared_state: Default::default(),
            sub_task_manager: SubTaskManager::default(),
            parent_cancellation_token: None,
            settings_file: None,
            default_backend: None,
            sub_agent_timeout_seconds: 30.0,
            stream_callback: None,
            parent_log_handler: None,
            parent_event_handler: None,
            parent_execution_context: None,
            model_provider: None,
            parent_run_context: None,
            tool_policy: None,
            budget_limits: None,
        }
    }

    #[test]
    fn parent_lineage_prefers_public_then_execution_then_request_without_task_fallback() {
        let mut context = context();
        context.parent_run_context = Some(RunContext {
            run_id: "public-run".to_string(),
            ..RunContext::default()
        });
        context.parent_execution_context = Some(ExecutionContext {
            metadata: [("_vv_agent_run_id".to_string(), json!("execution-run"))]
                .into_iter()
                .collect(),
            ..ExecutionContext::default()
        });
        let mut request = SubTaskRequest::new("researcher", "Collect facts");
        request
            .metadata
            .insert("parent_run_id".to_string(), json!("request-run"));

        assert_eq!(
            resolve_sub_task_identity(&context, &request).parent_run_id,
            "public-run"
        );

        context.parent_run_context = None;
        assert_eq!(
            resolve_sub_task_identity(&context, &request).parent_run_id,
            "execution-run"
        );

        context.parent_execution_context = None;
        assert_eq!(
            resolve_sub_task_identity(&context, &request).parent_run_id,
            "request-run"
        );

        request.metadata.remove("parent_run_id");
        assert_eq!(
            resolve_sub_task_identity(&context, &request).parent_run_id,
            ""
        );
    }

    #[test]
    fn trace_identity_prefers_execution_then_public_then_task_then_child_run() {
        let mut context = context();
        context
            .parent_task
            .metadata
            .insert("trace_id".to_string(), json!("parent-task-trace"));
        context.parent_run_context = Some(RunContext {
            metadata: [("trace_id".to_string(), json!("public-context-trace"))]
                .into_iter()
                .collect(),
            ..RunContext::default()
        });
        context.parent_execution_context = Some(ExecutionContext {
            metadata: [(
                "_vv_agent_trace_id".to_string(),
                json!("execution-context-trace"),
            )]
            .into_iter()
            .collect(),
            ..ExecutionContext::default()
        });
        let mut request = SubTaskRequest::new("researcher", "Collect facts");
        request
            .metadata
            .insert("run_id".to_string(), json!("spoof-child-run"));

        assert_eq!(
            resolve_sub_task_identity(&context, &request).trace_id,
            "execution-context-trace"
        );

        context.parent_execution_context = Some(ExecutionContext {
            metadata: [
                ("_vv_agent_trace_id".to_string(), json!("  ")),
                ("trace_id".to_string(), json!("execution-public-trace")),
            ]
            .into_iter()
            .collect(),
            ..ExecutionContext::default()
        });
        assert_eq!(
            resolve_sub_task_identity(&context, &request).trace_id,
            "execution-public-trace"
        );

        context.parent_execution_context = Some(ExecutionContext {
            metadata: [("trace_id".to_string(), json!("  "))]
                .into_iter()
                .collect(),
            ..ExecutionContext::default()
        });
        assert_eq!(
            resolve_sub_task_identity(&context, &request).trace_id,
            "public-context-trace"
        );

        context
            .parent_run_context
            .as_mut()
            .expect("public run context")
            .metadata
            .remove("trace_id");
        assert_eq!(
            resolve_sub_task_identity(&context, &request).trace_id,
            "parent-task-trace"
        );

        context.parent_task.metadata.remove("trace_id");
        let resolved = resolve_sub_task_identity(&context, &request);
        assert_eq!(resolved.trace_id, resolved.run_id);
        assert_ne!(resolved.run_id, "spoof-child-run");
    }

    #[test]
    fn request_metadata_cannot_assign_child_task_session_or_run_identity() {
        let context = context();
        let mut request = SubTaskRequest::new("researcher", "Collect facts");
        request.metadata.extend([
            ("task_id".to_string(), json!("spoof-task")),
            ("session_id".to_string(), json!("spoof-session")),
            ("run_id".to_string(), json!("spoof-run")),
        ]);

        let resolved = resolve_sub_task_identity(&context, &request);

        assert_ne!(resolved.task_id, "spoof-task");
        assert_ne!(resolved.session_id, "spoof-session");
        assert_ne!(resolved.run_id, "spoof-run");
        assert_eq!(resolved.session_id, resolved.task_id);
    }

    #[test]
    fn non_string_identity_metadata_is_ignored_and_falls_through() {
        let contract: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/parity/configured_sub_agent_v1.json"
        )))
        .expect("configured sub-agent contract");
        assert_eq!(
            contract["identity"]["non_string_metadata_policy"],
            "ignore_and_fall_through"
        );

        for invalid in contract["identity"]["non_string_metadata_values"]
            .as_array()
            .expect("non-string metadata values")
        {
            let mut context = context();
            context
                .parent_task
                .metadata
                .insert("trace_id".to_string(), invalid.clone());
            context.parent_run_context = Some(RunContext {
                metadata: [("trace_id".to_string(), invalid.clone())]
                    .into_iter()
                    .collect(),
                ..RunContext::default()
            });
            context.parent_execution_context = Some(ExecutionContext {
                metadata: [
                    ("_vv_agent_run_id".to_string(), invalid.clone()),
                    ("_vv_agent_trace_id".to_string(), invalid.clone()),
                    ("trace_id".to_string(), invalid.clone()),
                ]
                .into_iter()
                .collect(),
                ..ExecutionContext::default()
            });
            let mut request = SubTaskRequest::new("researcher", "Collect facts");
            request.metadata.extend([
                ("parent_run_id".to_string(), invalid.clone()),
                ("parent_tool_call_id".to_string(), invalid.clone()),
            ]);

            let resolved = resolve_sub_task_identity(&context, &request);

            assert_eq!(resolved.trace_id, resolved.run_id);
            assert!(resolved.parent_run_id.is_empty());
            assert!(resolved.parent_tool_call_id.is_empty());
        }
    }
}
