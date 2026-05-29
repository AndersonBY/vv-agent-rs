use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::llm::LlmClient;
use crate::runtime::sub_task_manager::SubTaskManager;
use crate::runtime::AgentRuntime;
use crate::tools::SubTaskRunner;
use crate::types::{AgentTask, SubTaskOutcome, SubTaskRequest};
use crate::workspace::WorkspaceBackend;

use super::task::build_sub_agent_task;
use super::types::{SubTaskBuildInputs, SubTaskCallbacks, SubTaskRunContext};

mod identity;
mod model;
mod outcome;
mod session;

use identity::resolve_sub_task_identity;
use model::resolve_sub_agent_client;
use outcome::{failed_sub_task_outcome, record_sub_task_outcome};
use session::run_attached_sub_agent_session;

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
    let identity = resolve_sub_task_identity(parent_task, &request);

    let Some(sub_agent) = context.parent_task.sub_agents.get(&request.agent_name) else {
        let available = context
            .parent_task
            .sub_agents
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let outcome = failed_sub_task_outcome(
            &identity.task_id,
            &request.agent_name,
            &identity.session_id,
            format!(
                "Unknown sub-agent {:?}. Available: {available}",
                request.agent_name
            ),
        );
        return record_sub_task_outcome(&context, &identity.task_id, outcome);
    };

    let resolved_client = match resolve_sub_agent_client(&context, parent_task, sub_agent) {
        Ok(resolved) => resolved,
        Err(error) => {
            let outcome = failed_sub_task_outcome(
                &identity.task_id,
                &request.agent_name,
                &identity.session_id,
                error,
            );
            return record_sub_task_outcome(&context, &identity.task_id, outcome);
        }
    };

    let sub_task = build_sub_agent_task(
        &context,
        SubTaskBuildInputs {
            sub_task_id: &identity.task_id,
            sub_session_id: &identity.session_id,
            sub_agent_name: &request.agent_name,
            sub_agent,
            resolved_model_id: &resolved_client.model_id,
            request: &request,
        },
    );

    let outcome = match run_attached_sub_agent_session(
        &context,
        &request,
        &identity.task_id,
        &identity.session_id,
        sub_task,
        resolved_client,
    ) {
        Ok(outcome) => outcome,
        Err(error) => failed_sub_task_outcome(
            &identity.task_id,
            &request.agent_name,
            &identity.session_id,
            error,
        ),
    };
    record_sub_task_outcome(&context, &identity.task_id, outcome)
}
