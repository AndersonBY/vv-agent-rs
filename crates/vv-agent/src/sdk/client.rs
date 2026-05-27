use std::sync::Arc;

use crate::config::ResolvedModelConfig;
use crate::llm::{LlmClient, ScriptedLlmClient};
use crate::runtime::{AgentRuntime, ExecutionContext, RuntimeRunControls};
use crate::types::AgentTask;
use crate::workspace::LocalWorkspaceBackend;

use super::session::{AgentSession, AgentSessionRunRequest};
use super::types::{query_text_from_run, AgentDefinition, AgentRun, AgentSDKOptions};

#[derive(Clone)]
pub struct AgentSDKClient {
    pub options: AgentSDKOptions,
    default_agent: Option<AgentDefinition>,
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
        let execution_context = execution_context_from_request(&request);
        let mut task = task_from_definition(definition, request.prompt);
        task.initial_messages = request.initial_messages;
        task.initial_shared_state = request.shared_state;
        let result = self
            .run_with_controls(
                task,
                RuntimeRunControls {
                    log_handler: request.runtime_event_handler,
                    before_cycle_messages: None,
                    steering_queue: request.steering_queue,
                    cancellation_token: request.cancellation_token,
                    execution_context,
                },
            )
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
        let execution_context = execution_context_from_request(&request);
        let mut task = task_from_definition(definition, request.prompt);
        task.initial_messages = request.initial_messages;
        task.initial_shared_state = request.shared_state;
        runtime
            .run_with_controls(
                task,
                RuntimeRunControls {
                    log_handler: request.runtime_event_handler,
                    before_cycle_messages: None,
                    steering_queue: request.steering_queue,
                    cancellation_token: request.cancellation_token,
                    execution_context,
                },
            )
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

fn task_from_definition(definition: &AgentDefinition, prompt: String) -> AgentTask {
    let mut task = AgentTask::new(
        format!("{}-task", definition.model),
        definition.model.clone(),
        definition.system_prompt.clone().unwrap_or_default(),
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
    task.metadata = definition.metadata.clone();
    task
}

impl AgentSDKClient {
    pub fn new(options: AgentSDKOptions) -> Self {
        Self {
            options,
            default_agent: None,
            runtime: Arc::new(NullRunAgent),
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
        self.runtime = Arc::new(runtime);
        self
    }

    pub fn set_default_agent(&mut self, definition: AgentDefinition) {
        self.default_agent = Some(definition);
    }

    pub fn run_with_agent(
        &self,
        definition: AgentDefinition,
        prompt: impl Into<String>,
    ) -> Result<AgentRun, String> {
        let mut request = AgentSessionRunRequest::new(prompt);
        request.stream_callback = self.options.stream_callback.clone();
        self.runtime.run_with_session(&definition, request)
    }

    pub fn run(&self, prompt: impl Into<String>) -> Result<AgentRun, String> {
        let agent = self
            .default_agent
            .clone()
            .unwrap_or_else(|| AgentDefinition::default_for_model("demo"));
        self.run_with_agent(agent, prompt)
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
}

struct NullRunAgent;

impl RunAgent for NullRunAgent {
    fn run_with_session(
        &self,
        _definition: &AgentDefinition,
        _request: AgentSessionRunRequest,
    ) -> Result<AgentRun, String> {
        Err("runtime not configured".to_string())
    }
}

pub fn create_agent_session(
    client: &AgentSDKClient,
    agent_name: impl Into<String>,
    definition: AgentDefinition,
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
    AgentSession::new_with_context(
        execute_run,
        agent_name,
        definition,
        client.options.workspace.clone(),
    )
}

pub fn run(client: &AgentSDKClient, prompt: impl Into<String>) -> Result<AgentRun, String> {
    client.run(prompt)
}

pub fn query(client: &AgentSDKClient, prompt: impl Into<String>) -> Result<String, String> {
    client.query(prompt)
}
