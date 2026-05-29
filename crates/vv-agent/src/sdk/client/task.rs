mod build;
mod ids;
mod metadata;

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
