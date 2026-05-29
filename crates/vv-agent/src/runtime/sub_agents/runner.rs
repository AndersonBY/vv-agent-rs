use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::config::build_vv_llm_from_local_settings;
use crate::llm::LlmClient;
use crate::runtime::sub_agent_sessions::{
    register_sub_agent_session, unregister_sub_agent_session, SubAgentSession,
};
use crate::runtime::sub_task_manager::{SubTaskManager, SubTaskSessionAttachment};
use crate::runtime::AgentRuntime;
use crate::tools::SubTaskRunner;
use crate::types::{AgentStatus, AgentTask, SubAgentConfig, SubTaskOutcome, SubTaskRequest};
use crate::workspace::WorkspaceBackend;

use super::session::RuntimeSubAgentSession;
use super::task::build_sub_agent_task;
use super::types::{
    ResolvedSubAgentClient, RuntimeSubAgentSessionParts, SubTaskBuildInputs, SubTaskCallbacks,
    SubTaskRunContext,
};

impl<C: LlmClient + Clone + 'static> AgentRuntime<C> {
    pub(in crate::runtime) fn build_sub_task_runner(
        &self,
        parent_task: &AgentTask,
        workspace_path: PathBuf,
        workspace_backend: Arc<dyn WorkspaceBackend>,
        parent_shared_state: BTreeMap<String, Value>,
        sub_task_manager: SubTaskManager,
        callbacks: SubTaskCallbacks,
    ) -> Option<SubTaskRunner> {
        if parent_task.sub_agents.is_empty() {
            return None;
        }
        let llm_client: Arc<dyn LlmClient> = Arc::new(self.llm_client.clone());
        let tool_registry = self.tool_registry.clone();
        let parent_task = parent_task.clone();
        let sub_task_context = SubTaskRunContext {
            llm_client,
            tool_registry,
            workspace_backend,
            workspace_path,
            parent_task,
            parent_shared_state,
            sub_task_manager,
            settings_file: self.settings_file.clone(),
            default_backend: self.default_backend.clone(),
            sub_agent_timeout_seconds: self.sub_agent_timeout_seconds,
            stream_callback: callbacks.stream_callback,
            parent_log_handler: callbacks.parent_log_handler,
            parent_event_handler: callbacks.parent_event_handler,
        };
        Some(Arc::new(move |request| {
            run_sub_task(sub_task_context.clone(), request)
        }))
    }
}

fn run_sub_task(context: SubTaskRunContext, request: SubTaskRequest) -> SubTaskOutcome {
    let parent_task = &context.parent_task;
    let sub_task_id = request
        .metadata
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            SubTaskManager::next_task_identity(&parent_task.task_id, &request.agent_name).0
        });
    let sub_session_id = request
        .metadata
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| sub_task_id.clone());

    let Some(sub_agent) = context.parent_task.sub_agents.get(&request.agent_name) else {
        let agent_name = request.agent_name;
        let available = context
            .parent_task
            .sub_agents
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let outcome = SubTaskOutcome {
            task_id: sub_task_id.clone(),
            agent_name: agent_name.clone(),
            status: AgentStatus::Failed,
            session_id: Some(sub_session_id),
            final_answer: None,
            wait_reason: None,
            error: Some(format!(
                "Unknown sub-agent {agent_name:?}. Available: {available}"
            )),
            cycles: 0,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        };
        context
            .sub_task_manager
            .record_outcome(&sub_task_id, outcome.clone());
        return outcome;
    };

    let resolved_client = match resolve_sub_agent_client(&context, parent_task, sub_agent) {
        Ok(resolved) => resolved,
        Err(error) => {
            let outcome = SubTaskOutcome {
                task_id: sub_task_id.clone(),
                agent_name: request.agent_name,
                status: AgentStatus::Failed,
                session_id: Some(sub_session_id),
                final_answer: None,
                wait_reason: None,
                error: Some(error),
                cycles: 0,
                todo_list: Vec::new(),
                resolved: BTreeMap::new(),
            };
            context
                .sub_task_manager
                .record_outcome(&sub_task_id, outcome.clone());
            return outcome;
        }
    };

    let sub_task = build_sub_agent_task(
        &context,
        SubTaskBuildInputs {
            sub_task_id: &sub_task_id,
            sub_session_id: &sub_session_id,
            sub_agent_name: &request.agent_name,
            sub_agent,
            resolved_model_id: &resolved_client.model_id,
            request: &request,
        },
    );
    let initial_prompt = sub_task.user_prompt.clone();
    let resolved_payload = resolved_client.payload.clone();
    let session = Arc::new(RuntimeSubAgentSession::new(RuntimeSubAgentSessionParts {
        llm_client: resolved_client.llm_client,
        tool_registry: context.tool_registry.clone(),
        workspace_path: context.workspace_path.clone(),
        workspace_backend: context.workspace_backend.clone(),
        task_template: sub_task,
        agent_name: request.agent_name.clone(),
        session_id: sub_session_id.clone(),
        resolved: resolved_client.payload,
        stream_callback: context.stream_callback.clone(),
        parent_log_handler: context.parent_log_handler.clone(),
        parent_event_handler: context.parent_event_handler.clone(),
    }));
    let sub_agent_session: Arc<dyn SubAgentSession> = session.clone();
    context
        .sub_task_manager
        .attach_session_with_resolved(SubTaskSessionAttachment {
            task_id: sub_task_id.clone(),
            session_id: sub_session_id.clone(),
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
            ("model".to_string(), Value::String(resolved_client.model_id)),
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

    register_sub_agent_session(sub_session_id.clone(), sub_agent_session.clone());
    let outcome = match session.continue_run(&initial_prompt) {
        Ok(outcome) => outcome,
        Err(error) => {
            unregister_sub_agent_session(&sub_session_id);
            let outcome = SubTaskOutcome {
                task_id: sub_task_id.clone(),
                agent_name: request.agent_name,
                status: AgentStatus::Failed,
                session_id: Some(sub_session_id),
                final_answer: None,
                wait_reason: None,
                error: Some(error),
                cycles: 0,
                todo_list: Vec::new(),
                resolved: BTreeMap::new(),
            };
            context
                .sub_task_manager
                .record_outcome(&sub_task_id, outcome.clone());
            return outcome;
        }
    };
    unregister_sub_agent_session(&sub_session_id);
    context
        .sub_task_manager
        .record_outcome(&sub_task_id, outcome.clone());
    outcome
}

fn resolve_sub_agent_client(
    context: &SubTaskRunContext,
    parent_task: &AgentTask,
    sub_agent: &SubAgentConfig,
) -> Result<ResolvedSubAgentClient, String> {
    let requested_model = if sub_agent.model.trim().is_empty() {
        parent_task.model.clone()
    } else {
        sub_agent.model.clone()
    };

    if let Some(settings_file) = &context.settings_file {
        let backend = sub_agent
            .backend
            .clone()
            .or_else(|| context.default_backend.clone())
            .unwrap_or_else(|| "inline".to_string());
        let (client, resolved) = build_vv_llm_from_local_settings(
            settings_file,
            &backend,
            &requested_model,
            context.sub_agent_timeout_seconds,
        )
        .map_err(|error| error.to_string())?;
        let endpoint = resolved
            .endpoint()
            .map(|endpoint| endpoint.endpoint_id.clone())
            .unwrap_or_default();
        let resolved_payload = BTreeMap::from([
            ("backend".to_string(), resolved.backend.clone()),
            (
                "selected_model".to_string(),
                resolved.selected_model.clone(),
            ),
            ("model_id".to_string(), resolved.model_id.clone()),
            ("endpoint".to_string(), endpoint),
        ]);
        return Ok(ResolvedSubAgentClient {
            llm_client: Arc::new(client),
            model_id: resolved.model_id,
            payload: resolved_payload,
        });
    }

    if requested_model != parent_task.model {
        return Err(
            "Sub-agent model resolution requires runtime settings_file when sub-agent model differs from parent model."
                .to_string(),
        );
    }

    Ok(ResolvedSubAgentClient {
        llm_client: context.llm_client.clone(),
        model_id: parent_task.model.clone(),
        payload: BTreeMap::new(),
    })
}
