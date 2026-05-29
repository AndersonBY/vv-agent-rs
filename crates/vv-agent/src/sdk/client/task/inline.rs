use std::path::PathBuf;

use crate::types::AgentTask;

use super::super::super::session::AgentSessionRunRequest;
use super::super::super::types::AgentDefinition;
use super::super::AgentSDKClient;

impl AgentSDKClient {
    pub fn prepare_task_with_agent(
        &self,
        definition: AgentDefinition,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
    ) -> AgentTask {
        self.prepare_task_with_named_agent_in_workspace(
            "inline",
            definition,
            prompt,
            resolved_model_id,
            self.options.workspace.clone(),
            None::<String>,
        )
    }

    pub fn prepare_task_with_agent_request(
        &self,
        definition: AgentDefinition,
        request: AgentSessionRunRequest,
        resolved_model_id: impl Into<String>,
    ) -> AgentTask {
        self.prepare_task_with_named_agent_request("inline", definition, request, resolved_model_id)
    }

    pub fn prepare_task_with_agent_with_session_id(
        &self,
        definition: AgentDefinition,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> AgentTask {
        self.prepare_task_with_agent_in_workspace_with_session_id(
            definition,
            prompt,
            resolved_model_id,
            self.options.workspace.clone(),
            session_id,
        )
    }

    pub fn prepare_task_with_agent_in_workspace(
        &self,
        definition: AgentDefinition,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> AgentTask {
        self.prepare_task_with_named_agent_in_workspace(
            "inline",
            definition,
            prompt,
            resolved_model_id,
            workspace,
            None::<String>,
        )
    }

    pub fn prepare_task_with_agent_in_workspace_with_session_id(
        &self,
        definition: AgentDefinition,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        session_id: impl Into<String>,
    ) -> AgentTask {
        self.prepare_task_with_named_agent_in_workspace(
            "inline",
            definition,
            prompt,
            resolved_model_id,
            workspace,
            Some(session_id.into()),
        )
    }
}
