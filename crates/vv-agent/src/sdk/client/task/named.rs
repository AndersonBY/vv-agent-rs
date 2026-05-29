use std::path::PathBuf;

use crate::types::AgentTask;

use super::super::super::session::AgentSessionRunRequest;
use super::super::AgentSDKClient;

impl AgentSDKClient {
    pub fn prepare_task_for_agent(
        &self,
        agent_name: impl AsRef<str>,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(self.prepare_task_with_named_agent_in_workspace(
            agent_name,
            definition,
            prompt,
            resolved_model_id,
            self.options.workspace.clone(),
            None::<String>,
        ))
    }

    pub fn prepare_task_for_agent_with_request(
        &self,
        agent_name: impl AsRef<str>,
        request: AgentSessionRunRequest,
        resolved_model_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(self.prepare_task_with_named_agent_request(
            agent_name,
            definition,
            request,
            resolved_model_id,
        ))
    }

    pub fn prepare_task_for_agent_with_session_id(
        &self,
        agent_name: impl AsRef<str>,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(self.prepare_task_with_named_agent_in_workspace(
            agent_name,
            definition,
            prompt,
            resolved_model_id,
            self.options.workspace.clone(),
            Some(session_id.into()),
        ))
    }

    pub fn prepare_task_for_agent_in_workspace(
        &self,
        agent_name: impl AsRef<str>,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> Result<AgentTask, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(self.prepare_task_with_named_agent_in_workspace(
            agent_name,
            definition,
            prompt,
            resolved_model_id,
            workspace,
            None::<String>,
        ))
    }

    pub fn prepare_task_for_agent_in_workspace_with_session_id(
        &self,
        agent_name: impl AsRef<str>,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        session_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(self.prepare_task_with_named_agent_in_workspace(
            agent_name,
            definition,
            prompt,
            resolved_model_id,
            workspace,
            Some(session_id.into()),
        ))
    }
}
