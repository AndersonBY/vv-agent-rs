use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::config::{
    apply_resolved_model_limits, build_vv_llm_from_local_settings, ResolvedModelConfig,
};
use crate::llm::{LlmClient, ScriptedLlmClient};
use crate::prompt::{
    build_raw_system_prompt_sections, build_system_prompt_bundle_with_options,
    BuildSystemPromptOptions,
};
use crate::runtime::{AgentRuntime, ExecutionContext, RuntimeRunControls};
use crate::types::{AgentTask, Metadata};
use crate::workspace::{LocalWorkspaceBackend, WorkspaceBackend};

use super::resources::AgentResourceLoader;
use super::session::{AgentSession, AgentSessionRunRequest};
use super::types::{query_text_from_run, AgentDefinition, AgentRun, AgentSDKOptions, SdkLlmClient};

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

pub trait RunAgent {
    fn run(&self, definition: &AgentDefinition, prompt: String) -> Result<AgentRun, String> {
        self.run_with_session(definition, AgentSessionRunRequest::new(prompt))
    }

    fn run_with_session(
        &self,
        definition: &AgentDefinition,
        request: AgentSessionRunRequest,
    ) -> Result<AgentRun, String>;
}

impl<C: LlmClient + Clone + 'static> RunAgent for AgentRuntime<C> {
    fn run_with_session(
        &self,
        definition: &AgentDefinition,
        request: AgentSessionRunRequest,
    ) -> Result<AgentRun, String> {
        let controls = run_controls_from_request(&request);
        let mut task = task_from_definition(definition, request.prompt);
        merge_request_metadata(&mut task, request.metadata);
        task.initial_messages = request.initial_messages;
        task.initial_shared_state = request.shared_state;
        let result = self
            .run_with_controls(task, controls)
            .map_err(|err| err.to_string())?;
        let resolved = ResolvedModelConfig::new(
            definition
                .backend
                .clone()
                .unwrap_or_else(|| "moonshot".to_string()),
            definition.model.clone(),
            definition.model.clone(),
            definition.model.clone(),
            Vec::new(),
        );
        Ok(AgentRun {
            agent_name: definition.model.clone(),
            result,
            resolved,
        })
    }
}

impl RunAgent for ScriptedLlmClient {
    fn run_with_session(
        &self,
        definition: &AgentDefinition,
        request: AgentSessionRunRequest,
    ) -> Result<AgentRun, String> {
        let runtime = AgentRuntime::new(self.clone());
        let controls = run_controls_from_request(&request);
        let mut task = task_from_definition(definition, request.prompt);
        merge_request_metadata(&mut task, request.metadata);
        task.initial_messages = request.initial_messages;
        task.initial_shared_state = request.shared_state;
        runtime
            .run_with_controls(task, controls)
            .map_err(|err| err.to_string())
            .map(|result| AgentRun {
                agent_name: definition.model.clone(),
                resolved: ResolvedModelConfig::new(
                    definition
                        .backend
                        .clone()
                        .unwrap_or_else(|| "moonshot".to_string()),
                    definition.model.clone(),
                    definition.model.clone(),
                    definition.model.clone(),
                    Vec::new(),
                ),
                result,
            })
    }
}

fn execution_context_from_request(request: &AgentSessionRunRequest) -> Option<ExecutionContext> {
    request
        .stream_callback
        .clone()
        .map(|callback| ExecutionContext::default().with_stream_callback(callback))
}

fn run_controls_from_request(request: &AgentSessionRunRequest) -> RuntimeRunControls {
    RuntimeRunControls {
        log_handler: request.runtime_event_handler.clone(),
        before_cycle_messages: None,
        interruption_messages: None,
        steering_queue: request.steering_queue.clone(),
        cancellation_token: request.cancellation_token.clone(),
        execution_context: execution_context_from_request(request),
        workspace: request.workspace.clone(),
        workspace_backend: request.workspace.as_ref().map(|workspace| {
            Arc::new(LocalWorkspaceBackend::new(workspace.clone())) as Arc<dyn WorkspaceBackend>
        }),
    }
}

fn task_from_definition(definition: &AgentDefinition, prompt: String) -> AgentTask {
    let (system_prompt, system_prompt_sections) = system_prompt_from_definition(definition);
    let mut metadata = definition.metadata.clone();
    metadata
        .entry("language".to_string())
        .or_insert_with(|| Value::String(definition.language.clone()));
    if let Some(shell) = definition.bash_shell.as_ref() {
        metadata
            .entry("bash_shell".to_string())
            .or_insert_with(|| Value::String(shell.clone()));
    }
    if !definition.windows_shell_priority.is_empty() {
        metadata
            .entry("windows_shell_priority".to_string())
            .or_insert_with(|| {
                Value::Array(
                    definition
                        .windows_shell_priority
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                )
            });
    }
    if !definition.bash_env.is_empty() {
        metadata
            .entry("bash_env".to_string())
            .or_insert_with(|| serde_json::to_value(&definition.bash_env).unwrap_or(Value::Null));
    }
    if !definition.sub_agents.is_empty() {
        metadata
            .entry("sub_agent_names".to_string())
            .or_insert_with(|| {
                Value::Array(
                    definition
                        .sub_agents
                        .keys()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                )
            });
    }
    if !definition.skill_directories.is_empty() {
        metadata
            .entry("available_skills".to_string())
            .or_insert_with(|| {
                Value::Array(
                    definition
                        .skill_directories
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                )
            });
    }
    let mut task = AgentTask::new(
        format!("{}-task", definition.model),
        definition.model.clone(),
        system_prompt,
        prompt,
    );
    task.max_cycles = definition.max_cycles;
    task.memory_compact_threshold = definition.memory_compact_threshold;
    task.memory_threshold_percentage = definition.memory_threshold_percentage;
    task.no_tool_policy = definition.no_tool_policy;
    task.allow_interruption = definition.allow_interruption;
    task.use_workspace = definition.use_workspace;
    task.has_sub_agents = definition.enable_sub_agents;
    task.sub_agents = definition.sub_agents.clone();
    task.agent_type = definition.agent_type.clone();
    task.native_multimodal = definition.native_multimodal;
    task.extra_tool_names = definition.extra_tool_names.clone();
    task.exclude_tools = definition.exclude_tools.clone();
    task.metadata = metadata;
    if !system_prompt_sections.is_empty() {
        task.metadata
            .entry("system_prompt_sections".to_string())
            .or_insert(Value::Array(system_prompt_sections));
    }
    task
}

fn system_prompt_from_definition(definition: &AgentDefinition) -> (String, Vec<Value>) {
    if let Some(system_prompt) = definition.system_prompt.as_ref() {
        return (
            system_prompt.clone(),
            build_raw_system_prompt_sections(system_prompt),
        );
    }

    let available_sub_agents = definition
        .sub_agents
        .iter()
        .map(|(name, config)| (name.clone(), config.description.clone()))
        .collect();
    let available_skills = definition
        .metadata
        .get("available_skills")
        .cloned()
        .or_else(|| {
            (!definition.skill_directories.is_empty()).then(|| {
                Value::Array(
                    definition
                        .skill_directories
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                )
            })
        });
    let prompt_bundle = build_system_prompt_bundle_with_options(
        &definition.description,
        BuildSystemPromptOptions {
            language: definition.language.clone(),
            allow_interruption: definition.allow_interruption,
            use_workspace: definition.use_workspace,
            enable_todo_management: definition.enable_todo_management,
            agent_type: definition.agent_type.clone(),
            available_sub_agents,
            available_skills,
            ..BuildSystemPromptOptions::default()
        },
    );
    (prompt_bundle.prompt, prompt_bundle.sections)
}

impl AgentSDKClient {
    pub fn new(options: AgentSDKOptions) -> Self {
        let mut agents = BTreeMap::new();
        let mut prompt_templates = BTreeMap::new();
        let mut resource_skill_directories = Vec::new();
        let mut resource_diagnostics = Vec::new();

        if options.auto_discover_resources {
            let mut loader = AgentResourceLoader::new(options.workspace.clone());
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

    pub fn with_runtime<C: LlmClient + Clone + 'static>(
        mut self,
        mut runtime: AgentRuntime<C>,
    ) -> Self {
        if runtime.log_preview_chars.is_none() {
            runtime.log_preview_chars = self.options.log_preview_chars;
        }
        if runtime.default_workspace.is_none() {
            let workspace = self.options.workspace.clone();
            runtime.default_workspace = Some(workspace.clone());
            runtime.workspace_backend = Arc::new(LocalWorkspaceBackend::new(workspace));
        }
        runtime.hooks.extend(self.options.runtime_hooks.clone());
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

    pub fn run_agent(
        &self,
        agent_name: impl AsRef<str>,
        prompt: impl Into<String>,
    ) -> Result<AgentRun, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        self.run_named_agent(agent_name, definition, prompt)
    }

    fn run_named_agent(
        &self,
        agent_name: &str,
        definition: AgentDefinition,
        prompt: impl Into<String>,
    ) -> Result<AgentRun, String> {
        let definition = self.effective_definition(definition);
        let mut request = AgentSessionRunRequest::new(prompt);
        request.stream_callback = self.options.stream_callback.clone();
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

    pub fn prepare_task_for_agent(
        &self,
        agent_name: impl AsRef<str>,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let definition = self.get_agent(agent_name.as_ref().trim())?.clone();
        Ok(self.prepare_task_with_agent(definition, prompt, resolved_model_id))
    }

    pub fn prepare_task_with_agent(
        &self,
        definition: AgentDefinition,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
    ) -> AgentTask {
        let mut task = task_from_definition(&self.effective_definition(definition), prompt.into());
        task.model = resolved_model_id.into();
        task
    }

    pub fn run(&self, prompt: impl Into<String>) -> Result<AgentRun, String> {
        if let Some(agent) = self.default_agent.clone() {
            return self.run_named_agent("default", agent, prompt);
        }
        if self.agents.len() == 1 {
            let (name, definition) = self.agents.iter().next().expect("single agent");
            return self.run_named_agent(name, definition.clone(), prompt);
        }
        if self.agents.is_empty() {
            return Err(
                "No agent configured. Call run_with_agent(...) or register named agents first."
                    .to_string(),
            );
        }
        let available = self.list_agents().join(", ");
        Err(format!(
            "Multiple agents configured. Call run_agent(name, ...) with one of: {available}"
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

    fn get_agent(&self, agent_name: &str) -> Result<&AgentDefinition, String> {
        if agent_name.is_empty() {
            return Err("Agent name cannot be empty".to_string());
        }
        self.agents.get(agent_name).ok_or_else(|| {
            let available = self.list_agents().join(", ");
            format!("Unknown agent: {agent_name}. Available: {available}")
        })
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

#[derive(Clone)]
struct SettingsRunAgent {
    options: AgentSDKOptions,
}

impl RunAgent for SettingsRunAgent {
    fn run_with_session(
        &self,
        definition: &AgentDefinition,
        request: AgentSessionRunRequest,
    ) -> Result<AgentRun, String> {
        let backend = definition
            .backend
            .clone()
            .unwrap_or_else(|| self.options.default_backend.clone());
        let (llm, resolved) = build_llm_from_options(&self.options, &backend, &definition.model)?;
        let mut runtime = AgentRuntime::new(llm);
        configure_runtime_from_options(&mut runtime, &self.options);

        let controls = run_controls_from_request(&request);
        let mut task = task_from_definition(definition, request.prompt);
        task.model = resolved.model_id.clone();
        apply_resolved_model_limits(&mut task, &resolved);
        merge_request_metadata(&mut task, request.metadata);
        task.initial_messages = request.initial_messages;
        task.initial_shared_state = request.shared_state;
        let result = runtime
            .run_with_controls(task, controls)
            .map_err(|err| err.to_string())?;
        Ok(AgentRun {
            agent_name: definition.model.clone(),
            result,
            resolved,
        })
    }
}

fn build_llm_from_options(
    options: &AgentSDKOptions,
    backend: &str,
    model: &str,
) -> Result<(SdkLlmClient, ResolvedModelConfig), String> {
    if let Some(builder) = &options.llm_builder {
        return builder(
            options.settings_file.as_path(),
            backend,
            model,
            options.timeout_seconds,
        );
    }
    let (llm, resolved) = build_vv_llm_from_local_settings(
        &options.settings_file,
        backend,
        model,
        options.timeout_seconds,
    )
    .map_err(|err| err.to_string())?;
    Ok((Arc::new(llm), resolved))
}

fn configure_runtime_from_options<C: LlmClient + Clone + 'static>(
    runtime: &mut AgentRuntime<C>,
    options: &AgentSDKOptions,
) {
    if runtime.log_preview_chars.is_none() {
        runtime.log_preview_chars = options.log_preview_chars;
    }
    if runtime.default_workspace.is_none() {
        let workspace = options.workspace.clone();
        runtime.default_workspace = Some(workspace.clone());
        runtime.workspace_backend = Arc::new(LocalWorkspaceBackend::new(workspace));
    }
    runtime.hooks.extend(options.runtime_hooks.clone());
}

fn merge_request_metadata(task: &mut AgentTask, metadata: Metadata) {
    for (key, value) in metadata {
        task.metadata.entry(key).or_insert(value);
    }
}

pub fn create_agent_session(
    client: &AgentSDKClient,
    agent_name: impl Into<String>,
    definition: AgentDefinition,
) -> AgentSession {
    create_agent_session_with_workspace(
        client,
        agent_name,
        definition,
        client.options.workspace.clone(),
    )
}

pub fn create_agent_session_with_workspace(
    client: &AgentSDKClient,
    agent_name: impl Into<String>,
    definition: AgentDefinition,
    workspace: impl Into<PathBuf>,
) -> AgentSession {
    let runtime = client.runtime.clone();
    let definition_for_run = definition.clone();
    let stream_callback = client.options.stream_callback.clone();
    let execute_run = Arc::new(move |mut request: AgentSessionRunRequest| {
        if request.stream_callback.is_none() {
            request.stream_callback = stream_callback.clone();
        }
        runtime.run_with_session(&definition_for_run, request)
    });
    AgentSession::new_with_context(execute_run, agent_name, definition, workspace)
}

pub fn create_agent_session_with_id(
    client: &AgentSDKClient,
    agent_name: impl Into<String>,
    definition: AgentDefinition,
    session_id: impl Into<String>,
) -> AgentSession {
    create_agent_session_with_id_and_workspace(
        client,
        agent_name,
        definition,
        session_id,
        client.options.workspace.clone(),
    )
}

pub fn create_agent_session_with_id_and_workspace(
    client: &AgentSDKClient,
    agent_name: impl Into<String>,
    definition: AgentDefinition,
    session_id: impl Into<String>,
    workspace: impl Into<PathBuf>,
) -> AgentSession {
    let runtime = client.runtime.clone();
    let definition_for_run = definition.clone();
    let stream_callback = client.options.stream_callback.clone();
    let execute_run = Arc::new(move |mut request: AgentSessionRunRequest| {
        if request.stream_callback.is_none() {
            request.stream_callback = stream_callback.clone();
        }
        runtime.run_with_session(&definition_for_run, request)
    });
    AgentSession::new_with_context_and_session_id(
        execute_run,
        session_id,
        agent_name,
        definition,
        workspace,
    )
}

pub fn run(client: &AgentSDKClient, prompt: impl Into<String>) -> Result<AgentRun, String> {
    client.run(prompt)
}

pub fn query(client: &AgentSDKClient, prompt: impl Into<String>) -> Result<String, String> {
    client.query(prompt)
}
