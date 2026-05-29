mod runtime;
mod sessions;
mod task;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::llm::LlmClient;
use crate::runtime::AgentRuntime;
use crate::types::{AgentTask, Metadata};

use super::resources::AgentResourceLoader;
use super::session::{AgentSession, AgentSessionRunRequest};
use super::types::{query_text_from_run, AgentDefinition, AgentRun, AgentSDKOptions};
pub use runtime::RunAgent;
use runtime::{configure_runtime_from_options, SettingsRunAgent};
pub use sessions::{
    create_agent_session, create_agent_session_with_id, create_agent_session_with_id_and_workspace,
    create_agent_session_with_id_and_workspace_and_shared_state,
    create_agent_session_with_shared_state, create_agent_session_with_workspace,
    create_agent_session_with_workspace_and_shared_state,
};
use task::{
    merge_request_metadata, normalize_prepare_session_id, task_from_definition_with_task_name,
};

#[derive(Clone)]
pub struct AgentSDKClient {
    pub options: AgentSDKOptions,
    default_agent: Option<AgentDefinition>,
    agents: BTreeMap<String, AgentDefinition>,
    prompt_templates: BTreeMap<String, String>,
    resource_skill_directories: Vec<String>,
    resource_diagnostics: Vec<String>,
    runtime: Arc<dyn RunAgent + Send + Sync>,
}

impl AgentSDKClient {
    pub fn new(options: AgentSDKOptions) -> Self {
        let mut agents = BTreeMap::new();
        let mut prompt_templates = BTreeMap::new();
        let mut resource_skill_directories = Vec::new();
        let mut resource_diagnostics = Vec::new();

        if options.auto_discover_resources {
            let mut loader = options
                .resource_loader
                .clone()
                .unwrap_or_else(|| AgentResourceLoader::new(options.workspace.clone()));
            let discovered = loader.discover();
            agents = discovered.agents;
            prompt_templates = discovered.prompts;
            resource_skill_directories = discovered.skill_directories;
            resource_diagnostics = discovered.diagnostics;
        }

        let runtime_options = options.clone();

        Self {
            options,
            default_agent: None,
            agents,
            prompt_templates,
            resource_skill_directories,
            resource_diagnostics,
            runtime: Arc::new(SettingsRunAgent {
                options: runtime_options,
            }),
        }
    }

    pub fn new_with_agent(options: AgentSDKOptions, agent: AgentDefinition) -> Self {
        let mut client = Self::new(options);
        client.default_agent = Some(agent);
        client
    }

    pub fn new_with_agents(
        options: AgentSDKOptions,
        agents: BTreeMap<String, AgentDefinition>,
    ) -> Result<Self, String> {
        let mut client = Self::new(options);
        client.register_agents(agents)?;
        Ok(client)
    }

    pub fn with_runtime<C: LlmClient + Clone + 'static>(
        mut self,
        mut runtime: AgentRuntime<C>,
    ) -> Self {
        configure_runtime_from_options(&mut runtime, &self.options);
        self.runtime = Arc::new(runtime);
        self
    }

    pub fn set_default_agent(&mut self, definition: AgentDefinition) {
        self.default_agent = Some(definition);
    }

    pub fn register_agent(
        &mut self,
        name: impl Into<String>,
        definition: AgentDefinition,
    ) -> Result<(), String> {
        let name = name.into().trim().to_string();
        if name.is_empty() {
            return Err("Agent name cannot be empty".to_string());
        }
        self.agents.insert(name, definition);
        Ok(())
    }

    pub fn register_agents(
        &mut self,
        agents: BTreeMap<String, AgentDefinition>,
    ) -> Result<(), String> {
        for (name, definition) in agents {
            self.register_agent(name, definition)?;
        }
        Ok(())
    }

    pub fn list_agents(&self) -> Vec<String> {
        self.agents.keys().cloned().collect()
    }

    pub fn resource_diagnostics(&self) -> Vec<String> {
        self.resource_diagnostics.clone()
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

    pub fn create_session(
        &self,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
    ) -> AgentSession {
        create_agent_session(self, agent_name, definition)
    }

    pub fn create_session_with_shared_state(
        &self,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        shared_state: Metadata,
    ) -> AgentSession {
        create_agent_session_with_shared_state(self, agent_name, definition, shared_state)
    }

    pub fn create_session_with_id(
        &self,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        session_id: impl Into<String>,
    ) -> AgentSession {
        create_agent_session_with_id(self, agent_name, definition, session_id)
    }

    pub fn create_session_with_workspace(
        &self,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
    ) -> AgentSession {
        create_agent_session_with_workspace(self, agent_name, definition, workspace)
    }

    pub fn create_session_with_workspace_and_shared_state(
        &self,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
        shared_state: Metadata,
    ) -> AgentSession {
        create_agent_session_with_workspace_and_shared_state(
            self,
            agent_name,
            definition,
            workspace,
            shared_state,
        )
    }

    pub fn create_session_with_id_and_workspace(
        &self,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> AgentSession {
        create_agent_session_with_id_and_workspace(
            self, agent_name, definition, session_id, workspace,
        )
    }

    pub fn create_session_with_id_workspace_and_shared_state(
        &self,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        shared_state: Metadata,
    ) -> AgentSession {
        create_agent_session_with_id_and_workspace_and_shared_state(
            self,
            agent_name,
            definition,
            session_id,
            workspace,
            shared_state,
        )
    }

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

    pub fn create_default_session(&self) -> Result<AgentSession, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call create_session_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call create_agent_session_by_name(name) with one of:",
        )?;
        Ok(create_agent_session(self, name, definition))
    }

    pub fn create_default_session_with_workspace(
        &self,
        workspace: impl Into<PathBuf>,
    ) -> Result<AgentSession, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call create_session_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call create_agent_session_by_name_in_workspace(name, workspace) with one of:",
        )?;
        Ok(create_agent_session_with_workspace(
            self, name, definition, workspace,
        ))
    }

    pub fn create_default_session_with_id(
        &self,
        session_id: impl Into<String>,
    ) -> Result<AgentSession, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call create_session_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call create_agent_session_by_name_with_id(name, session_id) with one of:",
        )?;
        Ok(create_agent_session_with_id(
            self, name, definition, session_id,
        ))
    }

    pub fn create_default_session_with_id_and_workspace(
        &self,
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> Result<AgentSession, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call create_session_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call create_agent_session_by_name_with_id_and_workspace(name, session_id, workspace) with one of:",
        )?;
        Ok(create_agent_session_with_id_and_workspace(
            self, name, definition, session_id, workspace,
        ))
    }

    pub fn create_default_session_with_workspace_and_shared_state(
        &self,
        workspace: impl Into<PathBuf>,
        shared_state: Metadata,
    ) -> Result<AgentSession, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call create_session_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call create_agent_session_by_name_in_workspace(name, workspace) with one of:",
        )?;
        Ok(create_agent_session_with_workspace_and_shared_state(
            self,
            name,
            definition,
            workspace,
            shared_state,
        ))
    }

    pub fn create_default_session_with_id_workspace_and_shared_state(
        &self,
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        shared_state: Metadata,
    ) -> Result<AgentSession, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call create_session_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call create_agent_session_by_name_with_id_and_workspace(name, session_id, workspace) with one of:",
        )?;
        Ok(create_agent_session_with_id_and_workspace_and_shared_state(
            self,
            name,
            definition,
            session_id,
            workspace,
            shared_state,
        ))
    }

    pub fn create_default_session_with_shared_state(
        &self,
        shared_state: Metadata,
    ) -> Result<AgentSession, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call create_session_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call create_agent_session_by_name_with_shared_state(name, shared_state) with one of:",
        )?;
        Ok(create_agent_session_with_shared_state(
            self,
            name,
            definition,
            shared_state,
        ))
    }

    pub fn create_agent_session_by_name(
        &self,
        agent_name: impl AsRef<str>,
    ) -> Result<AgentSession, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(create_agent_session(self, agent_name, definition))
    }

    pub fn create_agent_session_by_name_in_workspace(
        &self,
        agent_name: impl AsRef<str>,
        workspace: impl Into<PathBuf>,
    ) -> Result<AgentSession, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(create_agent_session_with_workspace(
            self, agent_name, definition, workspace,
        ))
    }

    pub fn create_agent_session_by_name_with_id(
        &self,
        agent_name: impl AsRef<str>,
        session_id: impl Into<String>,
    ) -> Result<AgentSession, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(create_agent_session_with_id(
            self, agent_name, definition, session_id,
        ))
    }

    pub fn create_agent_session_by_name_with_id_and_workspace(
        &self,
        agent_name: impl AsRef<str>,
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> Result<AgentSession, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(create_agent_session_with_id_and_workspace(
            self, agent_name, definition, session_id, workspace,
        ))
    }

    pub fn create_agent_session_by_name_in_workspace_with_shared_state(
        &self,
        agent_name: impl AsRef<str>,
        workspace: impl Into<PathBuf>,
        shared_state: Metadata,
    ) -> Result<AgentSession, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(create_agent_session_with_workspace_and_shared_state(
            self,
            agent_name,
            definition,
            workspace,
            shared_state,
        ))
    }

    pub fn create_agent_session_by_name_with_id_workspace_and_shared_state(
        &self,
        agent_name: impl AsRef<str>,
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        shared_state: Metadata,
    ) -> Result<AgentSession, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(create_agent_session_with_id_and_workspace_and_shared_state(
            self,
            agent_name,
            definition,
            session_id,
            workspace,
            shared_state,
        ))
    }

    pub fn create_agent_session_by_name_with_shared_state(
        &self,
        agent_name: impl AsRef<str>,
        shared_state: Metadata,
    ) -> Result<AgentSession, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(create_agent_session_with_shared_state(
            self,
            agent_name,
            definition,
            shared_state,
        ))
    }

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

    fn get_agent(&self, agent_name: &str) -> Result<&AgentDefinition, String> {
        if agent_name.is_empty() {
            return Err("Agent name cannot be empty".to_string());
        }
        self.agents.get(agent_name).ok_or_else(|| {
            let available = self.list_agents().join(", ");
            format!("Unknown agent: {agent_name}. Available: {available}")
        })
    }

    fn default_or_only_agent(
        &self,
        empty_message: &str,
        multiple_prefix: &str,
    ) -> Result<(String, AgentDefinition), String> {
        if let Some(agent) = self.default_agent.clone() {
            return Ok(("default".to_string(), agent));
        }
        if self.agents.len() == 1 {
            let (name, definition) = self.agents.iter().next().expect("single agent");
            return Ok((name.clone(), definition.clone()));
        }
        if self.agents.is_empty() {
            return Err(empty_message.to_string());
        }
        let available = self.list_agents().join(", ");
        Err(format!("{multiple_prefix} {available}"))
    }

    fn effective_definition(&self, mut definition: AgentDefinition) -> AgentDefinition {
        if self.options.bash_shell.is_some() && definition.bash_shell.is_none() {
            definition.bash_shell = self.options.bash_shell.clone();
        }
        if !self.options.windows_shell_priority.is_empty()
            && definition.windows_shell_priority.is_empty()
        {
            definition.windows_shell_priority = self.options.windows_shell_priority.clone();
        }
        if !self.options.bash_env.is_empty() {
            let mut bash_env = self.options.bash_env.clone();
            bash_env.extend(definition.bash_env.clone());
            definition.bash_env = bash_env;
        }
        if definition.system_prompt.is_none() {
            if let Some(template_name) = definition.system_prompt_template.as_deref() {
                if let Some(template) = self.prompt_templates.get(template_name) {
                    if !template.trim().is_empty() {
                        definition.description = template.clone();
                    }
                }
            }
        }
        if definition.skill_directories.is_empty() && !self.resource_skill_directories.is_empty() {
            definition.skill_directories = self.resource_skill_directories.clone();
        }
        definition
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
