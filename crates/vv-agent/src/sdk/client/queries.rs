use std::path::PathBuf;

use super::super::session::AgentSessionRunRequest;
use super::super::types::{query_text_from_run, AgentDefinition, AgentSDKOptions};
use super::AgentSDKClient;

impl AgentSDKClient {
    pub fn query(&self, prompt: impl Into<String>) -> Result<String, String> {
        self.query_with_require_completed(prompt, true)
    }

    pub fn query_with_require_completed(
        &self,
        prompt: impl Into<String>,
        require_completed: bool,
    ) -> Result<String, String> {
        let run = self.run(prompt)?;
        query_text_from_run(run, require_completed, "Agent query failed")
    }

    pub fn query_with_request(
        &self,
        request: AgentSessionRunRequest,
        require_completed: bool,
    ) -> Result<String, String> {
        let run = self.run_with_request(request)?;
        query_text_from_run(run, require_completed, "Agent query failed")
    }

    pub fn query_with_agent_request(
        &self,
        definition: AgentDefinition,
        request: AgentSessionRunRequest,
        require_completed: bool,
    ) -> Result<String, String> {
        let run = self.run_with_agent_request(definition, request)?;
        query_text_from_run(run, require_completed, "Agent query failed")
    }

    pub fn query_agent(
        &self,
        agent_name: impl AsRef<str>,
        prompt: impl Into<String>,
    ) -> Result<String, String> {
        self.query_agent_with_require_completed(agent_name, prompt, true)
    }

    pub fn query_agent_with_require_completed(
        &self,
        agent_name: impl AsRef<str>,
        prompt: impl Into<String>,
        require_completed: bool,
    ) -> Result<String, String> {
        let run = self.run_agent(agent_name, prompt)?;
        query_text_from_run(run, require_completed, "Agent query failed")
    }

    pub fn query_agent_with_request(
        &self,
        agent_name: impl AsRef<str>,
        request: AgentSessionRunRequest,
        require_completed: bool,
    ) -> Result<String, String> {
        let run = self.run_agent_with_request(agent_name, request)?;
        query_text_from_run(run, require_completed, "Agent query failed")
    }

    pub fn query_agent_in_workspace(
        &self,
        agent_name: impl AsRef<str>,
        prompt: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> Result<String, String> {
        self.query_agent_in_workspace_with_require_completed(agent_name, prompt, workspace, true)
    }

    pub fn query_agent_in_workspace_with_require_completed(
        &self,
        agent_name: impl AsRef<str>,
        prompt: impl Into<String>,
        workspace: impl Into<PathBuf>,
        require_completed: bool,
    ) -> Result<String, String> {
        let run = self.run_agent_in_workspace(agent_name, prompt, workspace)?;
        query_text_from_run(run, require_completed, "Agent query failed")
    }

    pub fn query_in_workspace(
        &self,
        prompt: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> Result<String, String> {
        self.query_in_workspace_with_require_completed(prompt, workspace, true)
    }

    pub fn query_in_workspace_with_require_completed(
        &self,
        prompt: impl Into<String>,
        workspace: impl Into<PathBuf>,
        require_completed: bool,
    ) -> Result<String, String> {
        let run = self.run_in_workspace(prompt, workspace)?;
        query_text_from_run(run, require_completed, "Agent query failed")
    }
}

pub fn query(client: &AgentSDKClient, prompt: impl Into<String>) -> Result<String, String> {
    client.query(prompt)
}

pub fn query_with_options_and_agent(
    options: AgentSDKOptions,
    definition: AgentDefinition,
    prompt: impl Into<String>,
) -> Result<String, String> {
    query_with_options_and_agent_with_require_completed(options, definition, prompt, true)
}

pub fn query_with_options_and_agent_with_require_completed(
    options: AgentSDKOptions,
    definition: AgentDefinition,
    prompt: impl Into<String>,
    require_completed: bool,
) -> Result<String, String> {
    let run = AgentSDKClient::new(options).run_with_agent(definition, prompt)?;
    query_text_from_run(run, require_completed, "Agent query failed")
}

pub fn query_with_options_and_agent_request(
    options: AgentSDKOptions,
    definition: AgentDefinition,
    request: AgentSessionRunRequest,
) -> Result<String, String> {
    query_with_options_and_agent_request_with_require_completed(options, definition, request, true)
}

pub fn query_with_options_and_agent_request_with_require_completed(
    options: AgentSDKOptions,
    definition: AgentDefinition,
    request: AgentSessionRunRequest,
    require_completed: bool,
) -> Result<String, String> {
    let run = AgentSDKClient::new(options).run_with_agent_request(definition, request)?;
    query_text_from_run(run, require_completed, "Agent query failed")
}

pub fn query_with_options_and_agent_in_workspace(
    options: AgentSDKOptions,
    definition: AgentDefinition,
    prompt: impl Into<String>,
    workspace: impl Into<PathBuf>,
) -> Result<String, String> {
    query_with_options_and_agent_in_workspace_with_require_completed(
        options, definition, prompt, workspace, true,
    )
}

pub fn query_with_options_and_agent_in_workspace_with_require_completed(
    options: AgentSDKOptions,
    definition: AgentDefinition,
    prompt: impl Into<String>,
    workspace: impl Into<PathBuf>,
    require_completed: bool,
) -> Result<String, String> {
    let run =
        AgentSDKClient::new(options).run_with_agent_in_workspace(definition, prompt, workspace)?;
    query_text_from_run(run, require_completed, "Agent query failed")
}
