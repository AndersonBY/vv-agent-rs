use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::runtime::sub_agent_sessions::{
    _unregister_sub_agent_session, register_sub_agent_session, SubAgentSession,
};
use crate::runtime::sub_task_manager::{SubTaskLineage, SubTaskSessionAttachment};
use crate::types::{AgentTask, SubTaskOutcome, SubTaskRequest};

use super::super::session::RuntimeSubAgentSession;
use super::super::types::{
    ResolvedSubAgentClient, RuntimeSubAgentSessionParts, SubRunLifecycle, SubTaskRunContext,
};

pub(super) fn run_attached_sub_agent_session(
    context: &SubTaskRunContext,
    request: &SubTaskRequest,
    lifecycle: &SubRunLifecycle,
    sub_task: AgentTask,
    resolved_client: ResolvedSubAgentClient,
) -> Result<SubTaskOutcome, String> {
    let initial_prompt = sub_task.user_prompt.clone();
    let mut tool_policy = context.tool_policy.clone().unwrap_or_default();
    if let Some(config) = context.parent_task.sub_agents.get(&request.agent_name) {
        tool_policy.extend_metadata_denials(&config.declared_tool_policy());
    }
    for exclusion in &sub_task.exclude_tools {
        if !tool_policy
            .disallowed_tools
            .iter()
            .any(|tool| tool == exclusion)
        {
            tool_policy.disallowed_tools.push(exclusion.clone());
        }
    }
    let resolved_payload = resolved_client.payload.clone();
    let model_id = resolved_client.model_id.clone();
    let session = Arc::new(RuntimeSubAgentSession::new(RuntimeSubAgentSessionParts {
        llm_client: resolved_client.llm_client,
        tool_registry: context.tool_registry.clone(),
        workspace_path: context.workspace_path.clone(),
        workspace_backend: context.workspace_backend.clone(),
        task_template: sub_task,
        agent_name: request.agent_name.clone(),
        session_id: lifecycle.session_id.clone(),
        resolved: resolved_client.payload,
        settings_file: context.settings_file.clone(),
        default_backend: context.default_backend.clone(),
        parent_cancellation_token: context.parent_cancellation_token.clone(),
        event_handler: context.event_handler.clone(),
        parent_execution_context: context.parent_execution_context.clone(),
        model_provider: context.model_provider.clone(),
        run_model_ref: resolved_client.run_model_ref,
        tool_policy,
        budget_limits: context.budget_limits.clone(),
        initial_lifecycle: lifecycle.clone(),
    }));
    let sub_agent_session: Arc<dyn SubAgentSession> = session.clone();
    context
        .sub_task_manager
        .attach_running_session_with_resolved_and_lineage(
            SubTaskSessionAttachment {
                task_id: lifecycle.task_id.clone(),
                session_id: lifecycle.session_id.clone(),
                agent_name: request.agent_name.clone(),
                task_title: request.task_description.clone(),
                workspace_backend: context.workspace_backend.clone(),
                session: sub_agent_session.clone(),
                resolved: resolved_payload,
            },
            SubTaskLineage {
                parent_run_id: (!lifecycle.parent_run_id.is_empty())
                    .then(|| lifecycle.parent_run_id.clone()),
                parent_tool_call_id: (!lifecycle.parent_tool_call_id.is_empty())
                    .then(|| lifecycle.parent_tool_call_id.clone()),
            },
        );
    session.emit(
        "session_created",
        BTreeMap::from([
            (
                "agent_name".to_string(),
                Value::String(request.agent_name.clone()),
            ),
            ("model".to_string(), Value::String(model_id)),
            (
                "workspace".to_string(),
                Value::String(context.workspace_path.display().to_string()),
            ),
            (
                "max_cycles".to_string(),
                Value::from(session.task_template.max_cycles as u64),
            ),
        ]),
    );

    let mut registration = SessionRegistration {
        session_id: lifecycle.session_id.clone(),
        session: sub_agent_session.clone(),
        registered: false,
    };
    register_sub_agent_session(lifecycle.session_id.clone(), sub_agent_session);
    registration.registered = true;
    session.continue_run(&initial_prompt)
}

struct SessionRegistration {
    session_id: String,
    session: Arc<dyn SubAgentSession>,
    registered: bool,
}

impl Drop for SessionRegistration {
    fn drop(&mut self) {
        if self.registered {
            _unregister_sub_agent_session(&self.session_id, Some(self.session.clone()));
        }
    }
}
