mod build;
mod defaults;
mod ids;
mod inline;
mod metadata;
mod named;

use std::path::PathBuf;

use serde_json::Value;

use crate::types::AgentTask;

use super::super::session::AgentSessionRunRequest;
use super::super::types::AgentDefinition;
use super::AgentSDKClient;

pub(super) use build::task_from_definition_with_task_name;
pub(super) use metadata::merge_request_metadata;
use metadata::normalize_prepare_session_id;

impl AgentSDKClient {
    fn prepare_task_with_named_agent_in_workspace(
        &self,
        agent_name: &str,
        definition: AgentDefinition,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        session_id: Option<String>,
    ) -> AgentTask {
        let mut request = AgentSessionRunRequest::new(prompt);
        request.workspace = Some(workspace.into());
        if let Some(session_id) = normalize_prepare_session_id(session_id) {
            request
                .metadata
                .entry("session_id".to_string())
                .or_insert(Value::String(session_id));
        }
        self.prepare_task_with_named_agent_request(
            agent_name,
            definition,
            request,
            resolved_model_id,
        )
    }

    fn prepare_task_with_named_agent_request(
        &self,
        agent_name: &str,
        definition: AgentDefinition,
        mut request: AgentSessionRunRequest,
        resolved_model_id: impl Into<String>,
    ) -> AgentTask {
        let workspace = request
            .workspace
            .take()
            .unwrap_or_else(|| self.options.workspace.clone());
        let task_name = request
            .task_name
            .as_deref()
            .map(str::trim)
            .filter(|task_name| !task_name.is_empty())
            .unwrap_or(agent_name);
        let mut task = task_from_definition_with_task_name(
            &self.effective_definition(definition),
            request.prompt,
            Some(workspace.as_path()),
            Some(task_name),
        );
        task.model = resolved_model_id.into();
        merge_request_metadata(&mut task, request.metadata);
        task
    }
}
