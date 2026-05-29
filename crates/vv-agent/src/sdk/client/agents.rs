use std::collections::BTreeMap;
use std::sync::Arc;

use crate::llm::LlmClient;
use crate::runtime::AgentRuntime;

use super::super::resources::AgentResourceLoader;
use super::super::types::{AgentDefinition, AgentSDKOptions};
use super::runtime::{configure_runtime_from_options, SettingsRunAgent};
use super::AgentSDKClient;

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

    pub(super) fn get_agent(&self, agent_name: &str) -> Result<&AgentDefinition, String> {
        if agent_name.is_empty() {
            return Err("Agent name cannot be empty".to_string());
        }
        self.agents.get(agent_name).ok_or_else(|| {
            let available = self.list_agents().join(", ");
            format!("Unknown agent: {agent_name}. Available: {available}")
        })
    }

    pub(super) fn default_or_only_agent(
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

    pub(super) fn effective_definition(&self, mut definition: AgentDefinition) -> AgentDefinition {
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
