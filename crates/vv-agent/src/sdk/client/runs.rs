use std::path::PathBuf;

use super::super::session::AgentSessionRunRequest;
use super::super::types::{AgentDefinition, AgentRun, AgentSDKOptions};
use super::AgentSDKClient;

impl AgentSDKClient {
    pub fn run(&self, prompt: impl Into<String>) -> Result<AgentRun, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call run_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call run_agent(name, ...) with one of:",
        )?;
        self.run_named_agent(&name, definition, prompt)
    }

    pub fn run_in_workspace(
        &self,
        prompt: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> Result<AgentRun, String> {
        let workspace = workspace.into();
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call run_with_agent_in_workspace(...) or register named agents first.",
            "Multiple agents configured. Call run_agent_in_workspace(name, ...) with one of:",
        )?;
        self.run_named_agent_with_workspace(&name, definition, prompt, Some(workspace))
    }

    pub fn run_with_agent(
        &self,
        definition: AgentDefinition,
        prompt: impl Into<String>,
    ) -> Result<AgentRun, String> {
        self.run_named_agent("inline", definition, prompt)
    }

    pub fn run_with_agent_in_workspace(
        &self,
        definition: AgentDefinition,
        prompt: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> Result<AgentRun, String> {
        self.run_named_agent_with_workspace("inline", definition, prompt, Some(workspace.into()))
    }

    pub fn run_with_agent_request(
        &self,
        definition: AgentDefinition,
        request: AgentSessionRunRequest,
    ) -> Result<AgentRun, String> {
        self.run_named_agent_with_request("inline", definition, request)
    }

    pub fn run_with_request(&self, request: AgentSessionRunRequest) -> Result<AgentRun, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call run_with_agent_request(...) or register named agents first.",
            "Multiple agents configured. Call run_agent_with_request(name, ...) with one of:",
        )?;
        self.run_named_agent_with_request(&name, definition, request)
    }

    pub fn run_agent(
        &self,
        agent_name: impl AsRef<str>,
        prompt: impl Into<String>,
    ) -> Result<AgentRun, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        self.run_named_agent(agent_name, definition, prompt)
    }

    pub fn run_agent_in_workspace(
        &self,
        agent_name: impl AsRef<str>,
        prompt: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> Result<AgentRun, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        self.run_named_agent_with_workspace(agent_name, definition, prompt, Some(workspace.into()))
    }

    pub fn run_agent_with_request(
        &self,
        agent_name: impl AsRef<str>,
        request: AgentSessionRunRequest,
    ) -> Result<AgentRun, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        self.run_named_agent_with_request(agent_name, definition, request)
    }

    fn run_named_agent(
        &self,
        agent_name: &str,
        definition: AgentDefinition,
        prompt: impl Into<String>,
    ) -> Result<AgentRun, String> {
        self.run_named_agent_with_workspace(agent_name, definition, prompt, None)
    }

    fn run_named_agent_with_workspace(
        &self,
        agent_name: &str,
        definition: AgentDefinition,
        prompt: impl Into<String>,
        workspace: Option<PathBuf>,
    ) -> Result<AgentRun, String> {
        let mut request = AgentSessionRunRequest::new(prompt);
        request.workspace = Some(workspace.unwrap_or_else(|| self.options.workspace.clone()));
        self.run_named_agent_with_request(agent_name, definition, request)
    }

    fn run_named_agent_with_request(
        &self,
        agent_name: &str,
        definition: AgentDefinition,
        mut request: AgentSessionRunRequest,
    ) -> Result<AgentRun, String> {
        let definition = self.effective_definition(definition);
        if request.workspace.is_none() {
            request.workspace = Some(self.options.workspace.clone());
        }
        if request.task_name.is_none() {
            request.task_name = Some(agent_name.to_string());
        }
        if request.stream_callback.is_none() {
            request.stream_callback = self.options.stream_callback.clone();
        }
        let mut run = self.runtime.run_with_session(&definition, request)?;
        run.agent_name = agent_name.to_string();
        Ok(run)
    }
}

pub fn run(client: &AgentSDKClient, prompt: impl Into<String>) -> Result<AgentRun, String> {
    client.run(prompt)
}

pub fn run_with_options_and_agent(
    options: AgentSDKOptions,
    definition: AgentDefinition,
    prompt: impl Into<String>,
) -> Result<AgentRun, String> {
    AgentSDKClient::new(options).run_with_agent(definition, prompt)
}

pub fn run_with_options_and_agent_in_workspace(
    options: AgentSDKOptions,
    definition: AgentDefinition,
    prompt: impl Into<String>,
    workspace: impl Into<PathBuf>,
) -> Result<AgentRun, String> {
    AgentSDKClient::new(options).run_with_agent_in_workspace(definition, prompt, workspace)
}

pub fn run_with_options_and_agent_request(
    options: AgentSDKOptions,
    definition: AgentDefinition,
    request: AgentSessionRunRequest,
) -> Result<AgentRun, String> {
    AgentSDKClient::new(options).run_with_agent_request(definition, request)
}
