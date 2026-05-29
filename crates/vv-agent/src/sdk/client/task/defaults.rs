use std::path::PathBuf;

use crate::types::AgentTask;

use super::super::super::session::AgentSessionRunRequest;
use super::super::AgentSDKClient;

impl AgentSDKClient {
    pub fn prepare_task(
        &self,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call prepare_task_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call prepare_task_for_agent(name, ...) with one of:",
        )?;
        Ok(self.prepare_task_with_named_agent_in_workspace(
            &name,
            definition,
            prompt,
            resolved_model_id,
            self.options.workspace.clone(),
            None::<String>,
        ))
    }

    pub fn prepare_task_with_request(
        &self,
        request: AgentSessionRunRequest,
        resolved_model_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call prepare_task_with_agent_request(...) or register named agents first.",
            "Multiple agents configured. Call prepare_task_for_agent_with_request(name, ...) with one of:",
        )?;
        Ok(self.prepare_task_with_named_agent_request(
            &name,
            definition,
            request,
            resolved_model_id,
        ))
    }

    pub fn prepare_task_with_session_id(
        &self,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call prepare_task_with_agent_with_session_id(...) or register named agents first.",
            "Multiple agents configured. Call prepare_task_for_agent_with_session_id(name, ...) with one of:",
        )?;
        Ok(self.prepare_task_with_named_agent_in_workspace(
            &name,
            definition,
            prompt,
            resolved_model_id,
            self.options.workspace.clone(),
            Some(session_id.into()),
        ))
    }

    pub fn prepare_task_in_workspace(
        &self,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> Result<AgentTask, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call prepare_task_with_agent_in_workspace(...) or register named agents first.",
            "Multiple agents configured. Call prepare_task_for_agent_in_workspace(name, ...) with one of:",
        )?;
        Ok(self.prepare_task_with_named_agent_in_workspace(
            &name,
            definition,
            prompt,
            resolved_model_id,
            workspace,
            None::<String>,
        ))
    }

    pub fn prepare_task_in_workspace_with_session_id(
        &self,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        session_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call prepare_task_with_agent_in_workspace_with_session_id(...) or register named agents first.",
            "Multiple agents configured. Call prepare_task_for_agent_in_workspace_with_session_id(name, ...) with one of:",
        )?;
        Ok(self.prepare_task_with_named_agent_in_workspace(
            &name,
            definition,
            prompt,
            resolved_model_id,
            workspace,
            Some(session_id.into()),
        ))
    }
}
