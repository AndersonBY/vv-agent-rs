use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::runtime::sub_agent_sessions::{
    register_sub_agent_session, unregister_sub_agent_session, SubAgentSession,
};
use crate::runtime::sub_task_manager::SubTaskSessionAttachment;
use crate::types::{AgentTask, SubTaskOutcome, SubTaskRequest};

use super::super::session::RuntimeSubAgentSession;
use super::super::types::{ResolvedSubAgentClient, RuntimeSubAgentSessionParts, SubTaskRunContext};

pub(super) fn run_attached_sub_agent_session(
    context: &SubTaskRunContext,
    request: &SubTaskRequest,
    sub_task_id: &str,
    sub_session_id: &str,
    sub_task: AgentTask,
    resolved_client: ResolvedSubAgentClient,
) -> Result<SubTaskOutcome, String> {
    let initial_prompt = sub_task.user_prompt.clone();
    let resolved_payload = resolved_client.payload.clone();
    let model_id = resolved_client.model_id.clone();
    let session = Arc::new(RuntimeSubAgentSession::new(RuntimeSubAgentSessionParts {
        llm_client: resolved_client.llm_client,
        tool_registry: context.tool_registry.clone(),
        workspace_path: context.workspace_path.clone(),
        workspace_backend: context.workspace_backend.clone(),
        task_template: sub_task,
        agent_name: request.agent_name.clone(),
        session_id: sub_session_id.to_string(),
        resolved: resolved_client.payload,
        stream_callback: context.stream_callback.clone(),
        parent_log_handler: context.parent_log_handler.clone(),
        parent_event_handler: context.parent_event_handler.clone(),
    }));
    let sub_agent_session: Arc<dyn SubAgentSession> = session.clone();
    context
        .sub_task_manager
        .attach_session_with_resolved(SubTaskSessionAttachment {
            task_id: sub_task_id.to_string(),
            session_id: sub_session_id.to_string(),
            agent_name: request.agent_name.clone(),
            task_title: request.task_description.clone(),
            workspace_backend: context.workspace_backend.clone(),
            session: sub_agent_session.clone(),
            resolved: resolved_payload,
        });
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

    register_sub_agent_session(sub_session_id.to_string(), sub_agent_session);
    let result = session.continue_run(&initial_prompt);
    unregister_sub_agent_session(sub_session_id);
    result
}
